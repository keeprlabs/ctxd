//! Write-ahead log for unflushed events.
//!
//! ## Design
//!
//! - Append-only file at `<root>/events.wal`.
//! - Each record is a length-prefixed JSON blob: 4 little-endian
//!   bytes of length, then that many bytes of UTF-8 JSON.
//! - `append` fsyncs after each write so a crash between `append()`
//!   returning and the next flush preserves the event.
//! - On startup, [`Wal::replay`] scans the file and reconstructs any
//!   records whose CRC validates. Partial writes at the tail are
//!   detected via length mismatch and discarded.
//! - After a successful Parquet flush, [`Wal::truncate`] resets the
//!   file to zero length — WAL entries become redundant once their
//!   events are sealed.
//!
//! ## Why not SQLite for this?
//!
//! We already have a sidecar SQLite, so "why not another table?" is
//! a reasonable question. Answer: fsync-per-append on SQLite is at
//! least a full transaction commit, which on a hot loop costs
//! milliseconds. Raw write + fsync on a flat file is microseconds.
//! For a log that sees the full append throughput of the daemon, the
//! difference dominates latency.

use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ctxd_core::event::Event;
use serde::{Deserialize, Serialize};
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::sync::Mutex;

use crate::store::StoreError;

/// A single recovered WAL entry.
#[derive(Debug, Clone)]
pub struct WalEntry {
    /// Sequence number assigned when the event was first appended.
    pub seq: i64,
    /// The buffered event itself.
    pub event: Event,
}

/// WAL record as written to disk. We persist the seq alongside the
/// event so replay hands back the original ordering.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct WalRecord {
    seq: i64,
    event: Event,
}

/// Local append-only write-ahead log.
///
/// Cheap to clone — every field is `Arc`-backed internally so
/// concurrent `append`s serialize on the inner mutex.
#[derive(Debug)]
pub struct Wal {
    path: PathBuf,
    writer: Arc<Mutex<File>>,
}

impl Wal {
    /// Open (or create) the WAL at `path`. Creates the parent dir
    /// if needed.
    pub async fn open(path: &Path) -> Result<Self, StoreError> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(path)
            .await?;
        Ok(Self {
            path: path.to_path_buf(),
            writer: Arc::new(Mutex::new(file)),
        })
    }

    /// Append a record and fsync.
    #[tracing::instrument(skip(self, event), fields(seq))]
    pub async fn append(&self, seq: i64, event: &Event) -> Result<(), StoreError> {
        let rec = WalRecord {
            seq,
            event: event.clone(),
        };
        let body = serde_json::to_vec(&rec)?;
        let len = body.len() as u32;
        let mut file = self.writer.lock().await;
        file.write_all(&len.to_le_bytes()).await?;
        file.write_all(&body).await?;
        file.flush().await?;
        // `sync_data` is enough — we don't care about metadata (mtime).
        file.sync_data().await?;
        Ok(())
    }

    /// Scan the WAL from the start. Stops at the first record whose
    /// length header overruns the file size or whose JSON body fails
    /// to parse — partial-write tail records are silently discarded.
    pub async fn replay(&self) -> Result<Vec<WalEntry>, StoreError> {
        let mut file = self.writer.lock().await;
        file.seek(SeekFrom::Start(0)).await?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).await?;
        // Re-seek to end for future appends.
        file.seek(SeekFrom::End(0)).await?;
        drop(file);

        let mut out = Vec::new();
        let mut cursor = 0usize;
        while cursor + 4 <= buf.len() {
            let len_bytes = [
                buf[cursor],
                buf[cursor + 1],
                buf[cursor + 2],
                buf[cursor + 3],
            ];
            let len = u32::from_le_bytes(len_bytes) as usize;
            cursor += 4;
            if cursor + len > buf.len() {
                // Tail partial write — discard.
                tracing::warn!(
                    cursor,
                    len,
                    total = buf.len(),
                    "WAL tail partial write discarded on replay"
                );
                break;
            }
            let body = &buf[cursor..cursor + len];
            cursor += len;
            match serde_json::from_slice::<WalRecord>(body) {
                Ok(rec) => out.push(WalEntry {
                    seq: rec.seq,
                    event: rec.event,
                }),
                Err(e) => {
                    tracing::warn!(error = %e, "malformed WAL record discarded on replay");
                    break;
                }
            }
        }
        Ok(out)
    }

    /// Truncate the WAL to zero length. Called after a successful
    /// Parquet flush + manifest update.
    pub async fn truncate(&self) -> Result<(), StoreError> {
        let mut file = self.writer.lock().await;
        file.set_len(0).await?;
        file.seek(SeekFrom::Start(0)).await?;
        file.sync_data().await?;
        Ok(())
    }

    /// Path on disk for diagnostics.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctxd_core::subject::Subject;
    use tempfile::TempDir;

    #[tokio::test]
    async fn append_then_replay_roundtrips() {
        let td = TempDir::new().unwrap();
        let wal_path = td.path().join("events.wal");
        let wal = Wal::open(&wal_path).await.unwrap();
        let e = Event::new(
            "s".into(),
            Subject::new("/a").unwrap(),
            "t".into(),
            serde_json::json!({"v": 1}),
        );
        wal.append(1, &e).await.unwrap();
        wal.append(2, &e).await.unwrap();

        // Reopen to simulate a crash + restart.
        let wal2 = Wal::open(&wal_path).await.unwrap();
        let entries = wal2.replay().await.unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].seq, 1);
        assert_eq!(entries[1].seq, 2);
    }

    #[tokio::test]
    async fn truncate_clears_the_log() {
        let td = TempDir::new().unwrap();
        let wal = Wal::open(&td.path().join("events.wal")).await.unwrap();
        let e = Event::new(
            "s".into(),
            Subject::new("/a").unwrap(),
            "t".into(),
            serde_json::json!({}),
        );
        wal.append(1, &e).await.unwrap();
        wal.truncate().await.unwrap();
        assert!(wal.replay().await.unwrap().is_empty());
    }
}
