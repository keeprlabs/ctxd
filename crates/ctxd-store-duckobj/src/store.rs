//! Core [`DuckObjStore`] implementation — the [`ctxd_store_core::Store`]
//! trait wiring lives in [`crate::store_trait`].
//!
//! The store holds:
//!
//! - A `Arc<dyn ObjectStore>` — durable backing. Local-fs, S3, Azure,
//!   GCS, R2 all go through the same API.
//! - A `Mutex<BufferState>` — unflushed events + a monotonic sequence
//!   counter sourced from the on-disk state on open.
//! - A `SqlitePool` — sidecar for KV / peers / caveats / vectors.
//! - A [`crate::wal::Wal`] — local append-only log so events survive
//!   a crash between append and Parquet rotation.

use chrono::{DateTime, Utc};
use ctxd_core::event::Event;
use ctxd_core::hash::PredecessorHash;
use ctxd_core::signing::EventSigner;
use ctxd_core::subject::Subject;
use object_store::local::LocalFileSystem;
use object_store::path::Path as ObjPath;
use object_store::ObjectStore;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::manifest::{Manifest, PartMeta};
use crate::parquet_io;
use crate::sidecar::Sidecar;
use crate::wal::Wal;

/// Errors from the DuckDB-on-object-store event store.
///
/// Variants wrap native `object_store`, SQLite, Arrow and Parquet
/// errors so callers can downcast when they need the original cause.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// Object-store call failed (PUT, GET, LIST, etc.).
    #[error("object store error: {0}")]
    ObjectStore(#[from] object_store::Error),

    /// A path supplied by the caller could not be parsed into an
    /// [`ObjPath`].
    #[error("invalid object store path: {0}")]
    BadPath(#[from] object_store::path::Error),

    /// SQLite error from the sidecar database.
    #[error("sidecar sqlite error: {0}")]
    Sidecar(#[from] sqlx::Error),

    /// Parquet read/write error.
    #[error("parquet error: {0}")]
    Parquet(#[from] parquet::errors::ParquetError),

    /// Arrow schema / record batch error.
    #[error("arrow error: {0}")]
    Arrow(#[from] arrow::error::ArrowError),

    /// JSON serialization error (when encoding `data` into the
    /// Parquet BYTES column).
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// Hash chain integrity violation.
    #[error("hash chain violation: expected {expected}, got {actual}")]
    HashChainViolation {
        /// Expected predecessor hash.
        expected: String,
        /// Actual predecessor hash.
        actual: String,
    },

    /// Subject path error.
    #[error("subject error: {0}")]
    Subject(#[from] ctxd_core::subject::SubjectError),

    /// I/O error from the local WAL file.
    #[error("wal io error: {0}")]
    Io(#[from] std::io::Error),

    /// A decoded value did not match the expected shape.
    #[error("decode error: {0}")]
    Decode(String),
}

/// Construction-time configuration for [`DuckObjStore`].
///
/// Rotation thresholds are tunable but callers should only override
/// them after reading the benchmark notes in `docs/storage-duckdb-object.md`.
#[derive(Debug, Clone)]
pub struct DuckObjConfig {
    /// Rotate the in-memory buffer into a new Parquet part after
    /// this many buffered events. Default: 1000.
    pub rotate_events: usize,
    /// Rotate when the serialized buffer would exceed this many bytes.
    /// Default: 16 MiB (well under the 64 MiB part cap).
    pub rotate_bytes: usize,
    /// Rotate after this many milliseconds have elapsed since the
    /// first event in the current buffer, regardless of size. Default:
    /// 1000 ms (1 second).
    pub rotate_interval_ms: u64,
    /// Soft cap on a single Parquet part. We never exceed this per
    /// the `rotate_bytes` guard; this field exists so admin tooling
    /// can verify files on disk. Default: 64 MiB.
    pub max_part_bytes: u64,
}

impl Default for DuckObjConfig {
    fn default() -> Self {
        Self {
            rotate_events: 1_000,
            rotate_bytes: 16 * 1024 * 1024,
            rotate_interval_ms: 1_000,
            max_part_bytes: 64 * 1024 * 1024,
        }
    }
}

/// The buffered-but-unflushed state of the store.
///
/// All fields are behind a `Mutex` on the outer struct. `seq` is the
/// next sequence number to assign; on open we initialize it from the
/// highest seq observed in the manifest (or 0 if brand new).
#[derive(Debug)]
pub(crate) struct BufferState {
    /// Next sequence number to hand out.
    pub next_seq: i64,
    /// Events in the buffer, in seq order.
    pub events: Vec<(i64, Event)>,
    /// Approximate serialized size of `events`. Cheap running total —
    /// we add `sizeof-json(data)` + a per-event constant on push and
    /// reset on rotation.
    pub approx_bytes: usize,
    /// Timestamp the first event entered the current buffer. `None`
    /// means the buffer is empty and the rotation timer is not armed.
    pub first_event_at: Option<std::time::Instant>,
}

impl BufferState {
    fn new(next_seq: i64) -> Self {
        Self {
            next_seq,
            events: Vec::new(),
            approx_bytes: 0,
            first_event_at: None,
        }
    }

    fn push(&mut self, event: Event) -> i64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        let size_hint = estimate_size(&event);
        self.events.push((seq, event));
        self.approx_bytes += size_hint;
        if self.first_event_at.is_none() {
            self.first_event_at = Some(std::time::Instant::now());
        }
        seq
    }

    fn drain(&mut self) -> Vec<(i64, Event)> {
        self.approx_bytes = 0;
        self.first_event_at = None;
        std::mem::take(&mut self.events)
    }
}

fn estimate_size(event: &Event) -> usize {
    // Rough approximation — we don't need exact bytes, just a trigger.
    64 + event.source.len()
        + event.subject.as_str().len()
        + event.event_type.len()
        + event.datacontenttype.len()
        + event.data.to_string().len()
        + event.predecessorhash.as_deref().map(str::len).unwrap_or(0)
        + event.signature.as_deref().map(str::len).unwrap_or(0)
        + event.attestation.as_deref().map(<[u8]>::len).unwrap_or(0)
        + event.parents.len() * 16
}

/// Event store backed by append-only Parquet on an [`ObjectStore`] with
/// a SQLite sidecar for KV / peers / caveats / vectors.
///
/// Cheap to clone — every field is `Arc`-backed internally.
pub struct DuckObjStore {
    object_store: Arc<dyn ObjectStore>,
    prefix: ObjPath,
    sidecar: Arc<Sidecar>,
    wal: Arc<Wal>,
    buffer: Arc<Mutex<BufferState>>,
    manifest: Arc<Mutex<Manifest>>,
    cfg: DuckObjConfig,
    signing_key: Option<Vec<u8>>,
    local_root: Option<PathBuf>,
}

impl std::fmt::Debug for DuckObjStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DuckObjStore")
            .field("prefix", &self.prefix)
            .field("has_signing_key", &self.signing_key.is_some())
            .field("local_root", &self.local_root)
            .finish()
    }
}

impl DuckObjStore {
    /// Open a store backed by a local-filesystem object store rooted
    /// at `root`. Convenience wrapper around [`DuckObjStore::open`].
    ///
    /// The sidecar SQLite lives at `<root>/sidecar.db`; the WAL lives
    /// at `<root>/events.wal`; Parquet parts land under
    /// `<root>/events/<subject_root>/<yyyy>-<mm>/part-<seq>-<uuid>.parquet`.
    pub async fn open_local(root: &Path) -> Result<Self, StoreError> {
        tokio::fs::create_dir_all(root).await?;
        let fs = LocalFileSystem::new_with_prefix(root)?;
        let sidecar_path = root.join("sidecar.db");
        let wal_path = root.join("events.wal");
        Self::open(
            Arc::new(fs),
            ObjPath::from(""),
            &sidecar_path,
            &wal_path,
            DuckObjConfig::default(),
            Some(root.to_path_buf()),
        )
        .await
    }

    /// Open a store with full control over backing object store and
    /// configuration.
    ///
    /// `sidecar_path` and `wal_path` are interpreted as *local*
    /// filesystem paths — they never cross the object store. That's
    /// intentional: the sidecar holds transactional state that
    /// Parquet can't model, and the WAL is the crash-safety anchor
    /// that must be locally durable.
    pub async fn open(
        object_store: Arc<dyn ObjectStore>,
        prefix: ObjPath,
        sidecar_path: &Path,
        wal_path: &Path,
        cfg: DuckObjConfig,
        local_root: Option<PathBuf>,
    ) -> Result<Self, StoreError> {
        let sidecar = Sidecar::open(sidecar_path).await?;
        let sidecar_arc = Arc::new(sidecar);

        // Load the manifest from the object store. If it's missing we
        // start with an empty one; if it exists we parse the sealed
        // parts so we know where to resume seq numbering.
        let manifest =
            Manifest::load(object_store.as_ref(), &prefix_events_manifest(&prefix)).await?;

        // Recover any WAL entries that were never flushed.
        let wal = Wal::open(wal_path).await?;
        let wal_entries = wal.replay().await?;

        // Dedup: events that the WAL knows about AND that are
        // already in sealed Parquet parts (by id) must not
        // double-count. The manifest only tracks (min/max seq, min/max
        // time, part path); it doesn't enumerate ids, so we fall back
        // to scanning the highest-seq Parquet part for a dedup set of
        // ids it contains. This is a cold-start cost; steady-state
        // path never touches it.
        let sealed_ids = if wal_entries.is_empty() {
            std::collections::HashSet::new()
        } else {
            parquet_io::collect_sealed_event_ids(object_store.as_ref(), &manifest).await?
        };

        // Highest seq in the manifest + length of any unsealed WAL
        // entries is where we resume.
        let max_sealed_seq = manifest.max_seq().unwrap_or(0);
        let mut buffer = BufferState::new(max_sealed_seq + 1);

        for entry in wal_entries {
            if sealed_ids.contains(&entry.event.id) {
                // Already sealed — skip. The WAL is append-only so
                // old entries may linger after a rotation; truncation
                // happens after a successful flush.
                continue;
            }
            // Use the WAL-recorded seq so the replayed order matches
            // what we promised the caller of `append` before the
            // crash.
            if entry.seq >= buffer.next_seq {
                buffer.next_seq = entry.seq + 1;
            }
            buffer.events.push((entry.seq, entry.event));
        }
        // Re-sort just in case WAL writes were racy across tasks.
        buffer.events.sort_by_key(|(s, _)| *s);
        if !buffer.events.is_empty() {
            buffer.first_event_at = Some(std::time::Instant::now());
            for (_, e) in &buffer.events {
                buffer.approx_bytes += estimate_size(e);
            }
        }

        Ok(Self {
            object_store,
            prefix,
            sidecar: sidecar_arc,
            wal: Arc::new(wal),
            buffer: Arc::new(Mutex::new(buffer)),
            manifest: Arc::new(Mutex::new(manifest)),
            cfg,
            signing_key: None,
            local_root,
        })
    }

    /// Install an Ed25519 signing key. Future appends without a
    /// pre-baked signature will be signed with this key before the
    /// event is committed to the WAL + buffer.
    pub fn set_signing_key(&mut self, key: Vec<u8>) {
        self.signing_key = Some(key);
    }

    /// Force a flush of the in-memory buffer to a new Parquet part.
    ///
    /// Returns the [`PartMeta`] of the freshly-sealed part (or `None`
    /// if the buffer was empty). Idempotent: calling on an empty
    /// buffer is a no-op.
    #[tracing::instrument(skip(self))]
    pub async fn flush(&self) -> Result<Option<PartMeta>, StoreError> {
        // Drain under the lock so concurrent appenders block briefly.
        let drained = {
            let mut buf = self.buffer.lock().await;
            if buf.events.is_empty() {
                return Ok(None);
            }
            buf.drain()
        };

        // Group by <subject_root>/<yyyy-mm> for partitioning. In
        // practice most batches fall into one group — we don't
        // optimize for the degenerate case where every event in the
        // buffer has a unique partition.
        let mut by_partition: std::collections::BTreeMap<(String, String), Vec<(i64, Event)>> =
            std::collections::BTreeMap::new();
        for (seq, event) in drained {
            let root = subject_root(&event.subject);
            let ym = format!("{}", event.time.format("%Y-%m"));
            by_partition
                .entry((root, ym))
                .or_default()
                .push((seq, event));
        }

        let mut last_meta: Option<PartMeta> = None;
        let mut manifest = self.manifest.lock().await;
        for ((root, ym), events) in by_partition {
            let part_uuid = Uuid::now_v7();
            let min_seq = events.first().map(|(s, _)| *s).unwrap_or(0);
            let max_seq = events.last().map(|(s, _)| *s).unwrap_or(0);
            let min_time = events
                .iter()
                .map(|(_, e)| e.time)
                .min()
                .unwrap_or_else(Utc::now);
            let max_time = events
                .iter()
                .map(|(_, e)| e.time)
                .max()
                .unwrap_or_else(Utc::now);
            let part_path = format!("events/{root}/{ym}/part-{min_seq:020}-{part_uuid}.parquet",);
            let obj_path = concat_path(&self.prefix, &part_path)?;

            parquet_io::write_part(self.object_store.as_ref(), &obj_path, &events).await?;

            let meta = PartMeta {
                path: part_path,
                subject_root: root,
                yyyy_mm: ym,
                min_seq,
                max_seq,
                min_time,
                max_time,
                event_count: events.len() as u64,
            };
            manifest.add_part(meta.clone());
            last_meta = Some(meta);
        }

        // Manifest write is the integrity boundary — write it
        // atomically (PUT with temp name + rename semantics where
        // supported; the LocalFileSystem impl is atomic-by-rename, and
        // object stores that support it use conditional PUT).
        manifest
            .save(
                self.object_store.as_ref(),
                &prefix_events_manifest(&self.prefix),
            )
            .await?;

        // WAL can now be truncated because everything in it is sealed.
        self.wal.truncate().await?;

        Ok(last_meta)
    }

    /// Append an event to the log.
    ///
    /// Writes to the WAL first, then pushes into the in-memory
    /// buffer. If the buffer crosses any rotation threshold we
    /// trigger a flush inline so the caller sees the rotation count
    /// the tests expect.
    #[tracing::instrument(skip(self, event), fields(subject = %event.subject))]
    pub async fn append(&self, mut event: Event) -> Result<Event, StoreError> {
        // Compute predecessor hash using sealed parts + buffer.
        if event.predecessorhash.is_none() {
            let last = self.last_event_for_subject(event.subject.as_str()).await?;
            if let Some(ref prev) = last {
                let hash = PredecessorHash::compute(prev).map_err(StoreError::Serialization)?;
                event.predecessorhash = Some(hash.to_string());
            }
        }

        // Sign if configured AND unsigned.
        if event.signature.is_none() {
            if let Some(ref key) = self.signing_key {
                match EventSigner::from_bytes(key) {
                    Ok(signer) => match signer.sign(&event) {
                        Ok(sig) => event.signature = Some(sig),
                        Err(e) => tracing::warn!(error = %e, "sign failed; appending unsigned"),
                    },
                    Err(e) => tracing::warn!(error = %e, "bad signing key; appending unsigned"),
                }
            }
        }

        // Canonical parents ordering so the byte-identical form is
        // stable across reads.
        event.parents = event.parents_sorted();

        // Assign seq under the buffer lock, then write WAL, then
        // push. Writing the WAL under the lock keeps WAL and buffer
        // ordering consistent, which matters for replay.
        let (seq, should_flush) = {
            let mut buf = self.buffer.lock().await;
            let seq = buf.push(event.clone());
            self.wal.append(seq, &event).await?;
            let should = buf.events.len() >= self.cfg.rotate_events
                || buf.approx_bytes >= self.cfg.rotate_bytes;
            (seq, should)
        };

        // Update the KV sidecar so kv_get reflects this append
        // immediately — federation LWW is on (time, id) so we don't
        // need to wait for the Parquet seal.
        self.sidecar
            .kv_upsert_lww(event.subject.as_str(), event.id, &event.data, event.time)
            .await?;

        // Record in the by-id index so read-your-writes stays fast
        // even across a restart before the next flush.
        // (The sidecar keeps this mirror; Parquet is the durable store.)
        self.sidecar.record_event_ref(seq, &event).await?;

        if should_flush {
            // Fire-and-join: the test expectation is that rotation
            // triggers synchronously on the size cap, so we flush
            // now. If flush fails, surface the error — the caller's
            // event is safe in the WAL either way.
            self.flush().await?;
        }
        Ok(event)
    }

    /// Poll for time-based rotation. Used by the benchmark harness
    /// and an optional background task to honour `rotate_interval_ms`.
    pub async fn maybe_flush_on_timer(&self) -> Result<Option<PartMeta>, StoreError> {
        let due = {
            let buf = self.buffer.lock().await;
            matches!(
                buf.first_event_at,
                Some(t) if t.elapsed().as_millis() as u64 >= self.cfg.rotate_interval_ms
            )
        };
        if due {
            self.flush().await
        } else {
            Ok(None)
        }
    }

    /// Read events for a subject, optionally recursive.
    ///
    /// Merges sealed Parquet parts with the in-memory buffer so that
    /// reads are read-your-writes consistent.
    #[tracing::instrument(skip(self), fields(subject = %subject))]
    pub async fn read(&self, subject: &Subject, recursive: bool) -> Result<Vec<Event>, StoreError> {
        let mut out = self
            .read_parquet_filtered(|e| matches_subject(e, subject, recursive))
            .await?;
        let buf = self.buffer.lock().await;
        for (_, e) in buf.events.iter() {
            if matches_subject(e, subject, recursive) {
                out.push(e.clone());
            }
        }
        // Already in seq order within each source; merge-sort by seq
        // would require remembering the seq alongside each event.
        // Instead we rely on the Parquet reader emitting events in
        // seq order AND the buffer being in seq order, AND the
        // sealed/buffer split being a clean break in seq space.
        Ok(out)
    }

    /// Read events for a subject as of a timestamp, optionally recursive.
    pub async fn read_at(
        &self,
        subject: &Subject,
        as_of: DateTime<Utc>,
        recursive: bool,
    ) -> Result<Vec<Event>, StoreError> {
        let mut out = self
            .read_parquet_filtered(|e| matches_subject(e, subject, recursive) && e.time <= as_of)
            .await?;
        let buf = self.buffer.lock().await;
        for (_, e) in buf.events.iter() {
            if matches_subject(e, subject, recursive) && e.time <= as_of {
                out.push(e.clone());
            }
        }
        Ok(out)
    }

    /// List distinct subjects. Sealed-parts contribute via a DuckDB-
    /// equivalent `SELECT DISTINCT subject`; the buffer contributes
    /// via a direct iteration.
    pub async fn subjects(
        &self,
        prefix: Option<&Subject>,
        recursive: bool,
    ) -> Result<Vec<String>, StoreError> {
        let mut set = std::collections::BTreeSet::new();
        let parquet = self
            .read_parquet_filtered(|e| match prefix {
                Some(p) => matches_subject(e, p, recursive),
                None => true,
            })
            .await?;
        for e in parquet {
            set.insert(e.subject.as_str().to_string());
        }
        let buf = self.buffer.lock().await;
        for (_, e) in buf.events.iter() {
            let keep = match prefix {
                Some(p) => matches_subject(e, p, recursive),
                None => true,
            };
            if keep {
                set.insert(e.subject.as_str().to_string());
            }
        }
        Ok(set.into_iter().collect())
    }

    /// Full-text search. DuckObj is read-optimized for analytical
    /// queries, not interactive FTS — we scan Parquet + buffer and
    /// substring-match on the JSON payload. Real FTS is v0.4.
    pub async fn search(
        &self,
        query: &str,
        limit: Option<usize>,
    ) -> Result<Vec<Event>, StoreError> {
        let needle = query.to_lowercase();
        let mut out = self
            .read_parquet_filtered(|e| payload_contains(e, &needle))
            .await?;
        {
            let buf = self.buffer.lock().await;
            for (_, e) in buf.events.iter() {
                if payload_contains(e, &needle) {
                    out.push(e.clone());
                }
            }
        }
        if let Some(lim) = limit {
            out.truncate(lim);
        }
        Ok(out)
    }

    /// Return the latest KV value from the sidecar.
    pub async fn kv_get(&self, subject: &str) -> Result<Option<serde_json::Value>, StoreError> {
        self.sidecar.kv_get(subject).await
    }

    /// KV value as of a timestamp — reconstructs from the event log
    /// (parquet + buffer) rather than the sidecar, so lagging LWW
    /// doesn't skew the answer.
    pub async fn kv_get_at(
        &self,
        subject: &str,
        as_of: DateTime<Utc>,
    ) -> Result<Option<serde_json::Value>, StoreError> {
        let events = self
            .read_parquet_filtered(|e| e.subject.as_str() == subject && e.time <= as_of)
            .await?;
        let mut latest: Option<Event> = None;
        for e in events {
            match &latest {
                None => latest = Some(e),
                Some(cur) if e.time > cur.time => latest = Some(e),
                _ => {}
            }
        }
        let buf = self.buffer.lock().await;
        for (_, e) in buf.events.iter() {
            if e.subject.as_str() == subject && e.time <= as_of {
                match &latest {
                    None => latest = Some(e.clone()),
                    Some(cur) if e.time > cur.time => latest = Some(e.clone()),
                    _ => {}
                }
            }
        }
        Ok(latest.map(|e| e.data))
    }

    /// Access the sidecar (used by the trait impl for peer / token /
    /// vector ops).
    pub(crate) fn sidecar(&self) -> Arc<Sidecar> {
        self.sidecar.clone()
    }

    async fn last_event_for_subject(&self, subject: &str) -> Result<Option<Event>, StoreError> {
        // Buffer first (newer by seq).
        {
            let buf = self.buffer.lock().await;
            for (_, e) in buf.events.iter().rev() {
                if e.subject.as_str() == subject {
                    return Ok(Some(e.clone()));
                }
            }
        }
        // Then parquet — scan matching parts only, in descending
        // (max_seq) order, returning the highest-seq event that
        // matches.
        let parts = {
            let m = self.manifest.lock().await;
            m.parts_for_subject(subject)
        };
        let mut best: Option<Event> = None;
        for part in parts {
            let obj = concat_path(&self.prefix, &part.path)?;
            let events = parquet_io::read_part(self.object_store.as_ref(), &obj).await?;
            for e in events {
                if e.subject.as_str() == subject {
                    match &best {
                        None => best = Some(e),
                        // Parquet rows come back in seq order inside a
                        // part; the last match wins for the part, and
                        // parts are iterated in manifest order.
                        _ => best = Some(e),
                    }
                }
            }
        }
        Ok(best)
    }

    async fn read_parquet_filtered<F>(&self, mut filter: F) -> Result<Vec<Event>, StoreError>
    where
        F: FnMut(&Event) -> bool,
    {
        let parts = {
            let m = self.manifest.lock().await;
            m.parts_snapshot()
        };
        let mut out = Vec::new();
        for part in parts {
            let obj = concat_path(&self.prefix, &part.path)?;
            let events = parquet_io::read_part(self.object_store.as_ref(), &obj).await?;
            for e in events {
                if filter(&e) {
                    out.push(e);
                }
            }
        }
        Ok(out)
    }
}

impl Clone for DuckObjStore {
    fn clone(&self) -> Self {
        Self {
            object_store: self.object_store.clone(),
            prefix: self.prefix.clone(),
            sidecar: self.sidecar.clone(),
            wal: self.wal.clone(),
            buffer: self.buffer.clone(),
            manifest: self.manifest.clone(),
            cfg: self.cfg.clone(),
            signing_key: self.signing_key.clone(),
            local_root: self.local_root.clone(),
        }
    }
}

/// Decide if `event` matches `subject` under the recursive rule.
pub(crate) fn matches_subject(event: &Event, subject: &Subject, recursive: bool) -> bool {
    let haystack = event.subject.as_str();
    let needle = subject.as_str();
    if haystack == needle {
        return true;
    }
    if !recursive {
        return false;
    }
    if needle == "/" {
        return true;
    }
    haystack.starts_with(&format!("{needle}/"))
}

fn payload_contains(event: &Event, needle_lower: &str) -> bool {
    event.data.to_string().to_lowercase().contains(needle_lower)
}

/// First non-empty path segment of the subject. Defaults to
/// `_root` for events anchored at `/`.
pub(crate) fn subject_root(subject: &Subject) -> String {
    let s = subject.as_str();
    let trimmed = s.trim_start_matches('/');
    match trimmed.split('/').next() {
        Some(seg) if !seg.is_empty() => seg.to_string(),
        _ => "_root".to_string(),
    }
}

fn prefix_events_manifest(prefix: &ObjPath) -> ObjPath {
    // Manifest path relative to the store root. `object_store` paths
    // are `/`-separated with no leading `/` — the concatenation
    // below honours that.
    if prefix.as_ref().is_empty() {
        ObjPath::from("events/_manifest.json")
    } else {
        ObjPath::from(format!("{}/events/_manifest.json", prefix.as_ref()))
    }
}

fn concat_path(prefix: &ObjPath, rel: &str) -> Result<ObjPath, StoreError> {
    let s = if prefix.as_ref().is_empty() {
        rel.to_string()
    } else {
        format!("{}/{}", prefix.as_ref(), rel)
    };
    Ok(ObjPath::parse(&s)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subject_root_defaults_to_root_for_leading_slash_only() {
        assert_eq!(subject_root(&Subject::new("/").unwrap()), "_root");
        assert_eq!(subject_root(&Subject::new("/foo").unwrap()), "foo");
        assert_eq!(subject_root(&Subject::new("/foo/bar").unwrap()), "foo");
    }

    #[test]
    fn matches_subject_recursive_rules() {
        let root = Subject::new("/r").unwrap();
        let match_a = Event::new(
            "x".into(),
            Subject::new("/r/a").unwrap(),
            "t".into(),
            serde_json::json!({}),
        );
        let no_match = Event::new(
            "x".into(),
            Subject::new("/rr").unwrap(),
            "t".into(),
            serde_json::json!({}),
        );
        assert!(matches_subject(&match_a, &root, true));
        assert!(!matches_subject(&match_a, &root, false));
        assert!(!matches_subject(&no_match, &root, true));
    }
}
