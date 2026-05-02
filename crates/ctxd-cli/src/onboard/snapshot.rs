//! Pre-flight snapshot + offboard restore.
//!
//! Before `onboard` mutates anything (writes plists, edits client
//! configs, mints caps), we capture the current state so `offboard`
//! can restore it. This converts offboard from "best-effort delete"
//! into "actually undo what we did."
//!
//! ## What we capture
//!
//! * **Running daemons** detected via the pidfile next to the DB.
//!   (Future: full `lsof`-style scan across the filesystem; v0.4
//!   sticks to the canonical pidfile path.)
//! * **Existing client config files** at the canonical paths
//!   ([`paths::claude_desktop_config`], [`paths::claude_code_config`])
//!   — read fully so the original file can be restored byte-for-byte.
//!
//! ## Where snapshots live
//!
//! `<data_dir>/onboard-snapshots/<timestamp>.json`. Multiple
//! snapshots accumulate so the user can inspect or roll back to any
//! prior onboard. `latest_snapshot()` returns the most recent.
//!
//! ## Restore semantics
//!
//! `restore()` writes each captured client config back, overwriting
//! whatever onboard's `configure-clients` step put there. It does
//! NOT touch the SQLite DB — that's data, not config, and `offboard
//! --purge` is the explicit knob for deleting it.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::onboard::paths;
use crate::pidfile;

/// Snapshot of the system state immediately before onboard mutates
/// anything. Written as JSON to `<data_dir>/onboard-snapshots/`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnboardSnapshot {
    /// Schema version. Bump on breaking changes to this struct.
    pub version: u8,
    /// RFC 3339 timestamp the snapshot was taken.
    pub captured_at: chrono::DateTime<chrono::Utc>,
    /// Pidfile content (if a daemon was already running).
    pub running_daemon: Option<RunningDaemonSnapshot>,
    /// Per-client config file before-state.
    pub client_configs: Vec<ClientConfigSnapshot>,
    /// SQLite DB path for which this snapshot was taken.
    pub db_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunningDaemonSnapshot {
    pub pid: u32,
    pub admin_bind: String,
    pub version: String,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub binary_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientConfigSnapshot {
    /// Stable client identifier (e.g. "claude-desktop", "claude-code").
    pub client: String,
    /// Path to the file we'd modify.
    pub path: PathBuf,
    /// Whether the file existed before onboard ran.
    pub existed: bool,
    /// File contents if it existed (raw bytes; UTF-8 typically but we
    /// don't enforce — the user might have BOMs or odd encoding).
    pub contents: Option<Vec<u8>>,
}

/// Capture a snapshot for `db_path`. Reads existing client configs,
/// queries the pidfile, returns a populated [`OnboardSnapshot`].
pub async fn capture(db_path: &Path) -> Result<OnboardSnapshot> {
    let running_daemon = capture_daemon(db_path).await;
    let client_configs = capture_clients()?;
    Ok(OnboardSnapshot {
        version: 1,
        captured_at: chrono::Utc::now(),
        running_daemon,
        client_configs,
        db_path: db_path.to_path_buf(),
    })
}

async fn capture_daemon(db_path: &Path) -> Option<RunningDaemonSnapshot> {
    use pidfile::DaemonState;
    match pidfile::detect(db_path).await {
        DaemonState::Running(pf) | DaemonState::Unresponsive(pf) => {
            // Best-effort: try to look up the binary path from /proc
            // (Linux) or `ps` (macOS). For v0.4 we leave this as None
            // and rely on pid + admin_bind for identification.
            Some(RunningDaemonSnapshot {
                pid: pf.pid,
                admin_bind: pf.admin_bind,
                version: pf.version,
                started_at: pf.started_at,
                binary_path: None,
            })
        }
        DaemonState::Stale(_) | DaemonState::NotRunning => None,
    }
}

fn capture_clients() -> Result<Vec<ClientConfigSnapshot>> {
    let mut snaps = Vec::new();
    let claude_desktop = paths::claude_desktop_config()?;
    snaps.push(snapshot_one("claude-desktop", &claude_desktop));
    let claude_code = paths::claude_code_config()?;
    snaps.push(snapshot_one("claude-code", &claude_code));
    // Codex doesn't have a stable config path we modify in v0.4
    // (we only write a snippet file to config_dir); skip it here.
    Ok(snaps)
}

fn snapshot_one(client: &str, path: &Path) -> ClientConfigSnapshot {
    match std::fs::read(path) {
        Ok(bytes) => ClientConfigSnapshot {
            client: client.to_string(),
            path: path.to_path_buf(),
            existed: true,
            contents: Some(bytes),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => ClientConfigSnapshot {
            client: client.to_string(),
            path: path.to_path_buf(),
            existed: false,
            contents: None,
        },
        Err(_) => ClientConfigSnapshot {
            // Permission error or similar — record the path and treat
            // as "didn't exist" for restore purposes (we won't try to
            // overwrite something we couldn't read).
            client: client.to_string(),
            path: path.to_path_buf(),
            existed: false,
            contents: None,
        },
    }
}

/// Persist the snapshot to `<data_dir>/onboard-snapshots/<ts>.json`.
/// Returns the absolute path of the snapshot file.
pub fn persist(snapshot: &OnboardSnapshot) -> Result<PathBuf> {
    let dir = paths::snapshots_dir()?;
    std::fs::create_dir_all(&dir).with_context(|| format!("create snapshot dir {dir:?}"))?;
    let ts = snapshot
        .captured_at
        .format("%Y-%m-%dT%H-%M-%SZ")
        .to_string();
    let path = dir.join(format!("{ts}.json"));
    let bytes = serde_json::to_vec_pretty(snapshot).context("serialize snapshot")?;
    std::fs::write(&path, &bytes).with_context(|| format!("write {path:?}"))?;
    Ok(path)
}

/// Locate the most recently persisted snapshot. Returns `None` if no
/// snapshot directory exists or it's empty.
pub fn latest_snapshot() -> Result<Option<PathBuf>> {
    let dir = match paths::snapshots_dir() {
        Ok(d) => d,
        Err(_) => return Ok(None),
    };
    if !dir.exists() {
        return Ok(None);
    }
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)
        .with_context(|| format!("read_dir {dir:?}"))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|s| s == "json").unwrap_or(false))
        .map(|e| e.path())
        .collect();
    // Filenames are RFC3339-shaped timestamps, so lexicographic sort
    // == chronological sort.
    entries.sort();
    Ok(entries.pop())
}

/// Read a snapshot file from disk.
pub fn read(path: &Path) -> Result<OnboardSnapshot> {
    let bytes = std::fs::read(path).with_context(|| format!("read snapshot {path:?}"))?;
    let snap: OnboardSnapshot =
        serde_json::from_slice(&bytes).with_context(|| format!("parse snapshot {path:?}"))?;
    Ok(snap)
}

/// Restore client config files from a snapshot. Returns counts of
/// what was restored / skipped.
pub fn restore(snapshot: &OnboardSnapshot) -> Result<RestoreReport> {
    let mut restored = Vec::new();
    let mut skipped = Vec::new();
    for s in &snapshot.client_configs {
        if s.existed {
            if let Some(bytes) = &s.contents {
                // Restore the original contents.
                std::fs::create_dir_all(s.path.parent().unwrap_or_else(|| Path::new("/"))).ok();
                let tmp_name = format!(".ctxd-restore-{}.tmp", std::process::id());
                let tmp = s
                    .path
                    .parent()
                    .map(|p| p.join(&tmp_name))
                    .unwrap_or_else(|| PathBuf::from(&tmp_name));
                std::fs::write(&tmp, bytes).with_context(|| format!("write {tmp:?}"))?;
                std::fs::rename(&tmp, &s.path)
                    .with_context(|| format!("rename {tmp:?} -> {:?}", s.path))?;
                restored.push(s.client.clone());
            } else {
                skipped.push(s.client.clone());
            }
        } else {
            // The file didn't exist before onboard ran. Remove it if
            // onboard's configure-clients step created it.
            match std::fs::remove_file(&s.path) {
                Ok(()) => restored.push(s.client.clone()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    skipped.push(s.client.clone());
                }
                Err(e) => {
                    return Err(anyhow::Error::new(e)
                        .context(format!("remove created config {:?}", s.path)));
                }
            }
        }
    }
    Ok(RestoreReport { restored, skipped })
}

/// Outcome of a restore pass.
#[derive(Debug, Clone, Default)]
pub struct RestoreReport {
    /// Client slugs whose config was restored or removed.
    pub restored: Vec<String>,
    /// Client slugs whose pre-state could not be restored (e.g. file
    /// was unreadable at capture time).
    pub skipped: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn capture_returns_a_v1_snapshot_with_db_path() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("ctxd.db");
        let snap = capture(&db).await.unwrap();
        assert_eq!(snap.version, 1);
        assert_eq!(snap.db_path, db);
        // No pidfile present, so no running daemon captured.
        assert!(snap.running_daemon.is_none());
    }

    #[test]
    fn snapshot_one_records_existed_when_file_present() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("c.json");
        std::fs::write(&p, b"hello").unwrap();
        let s = snapshot_one("test", &p);
        assert!(s.existed);
        assert_eq!(s.contents.as_deref(), Some(&b"hello"[..]));
    }

    #[test]
    fn snapshot_one_records_missing_when_file_absent() {
        let dir = tempfile::tempdir().unwrap();
        let s = snapshot_one("test", &dir.path().join("nope.json"));
        assert!(!s.existed);
        assert!(s.contents.is_none());
    }

    #[test]
    fn restore_writes_back_existing_contents() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("c.json");
        std::fs::write(&p, b"original").unwrap();
        let snap = OnboardSnapshot {
            version: 1,
            captured_at: chrono::Utc::now(),
            running_daemon: None,
            client_configs: vec![ClientConfigSnapshot {
                client: "test".into(),
                path: p.clone(),
                existed: true,
                contents: Some(b"original".to_vec()),
            }],
            db_path: dir.path().join("db"),
        };
        // Simulate onboard's modification.
        std::fs::write(&p, b"modified-by-onboard").unwrap();
        let report = restore(&snap).unwrap();
        assert_eq!(report.restored, vec!["test"]);
        assert_eq!(std::fs::read(&p).unwrap(), b"original");
    }

    #[test]
    fn restore_removes_files_that_didnt_exist_before() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("c.json");
        // File didn't exist at capture time:
        let snap = OnboardSnapshot {
            version: 1,
            captured_at: chrono::Utc::now(),
            running_daemon: None,
            client_configs: vec![ClientConfigSnapshot {
                client: "test".into(),
                path: p.clone(),
                existed: false,
                contents: None,
            }],
            db_path: dir.path().join("db"),
        };
        // Onboard created it:
        std::fs::write(&p, b"created-by-onboard").unwrap();
        let report = restore(&snap).unwrap();
        assert_eq!(report.restored, vec!["test"]);
        assert!(
            !p.exists(),
            "restore must remove a file that didn't exist before"
        );
    }

    #[test]
    fn restore_is_noop_when_target_already_matches_pre_state() {
        // File was missing before, onboard didn't create it (e.g.
        // user --skip-clients). Restore should be a no-op.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("missing.json");
        let snap = OnboardSnapshot {
            version: 1,
            captured_at: chrono::Utc::now(),
            running_daemon: None,
            client_configs: vec![ClientConfigSnapshot {
                client: "test".into(),
                path: p.clone(),
                existed: false,
                contents: None,
            }],
            db_path: dir.path().join("db"),
        };
        let report = restore(&snap).unwrap();
        // The file was already gone, so restore counts it as
        // skipped (no change needed).
        assert_eq!(report.skipped, vec!["test"]);
    }

    #[test]
    fn snapshot_round_trips_through_serde() {
        let snap = OnboardSnapshot {
            version: 1,
            captured_at: chrono::Utc::now(),
            running_daemon: Some(RunningDaemonSnapshot {
                pid: 1234,
                admin_bind: "127.0.0.1:7777".into(),
                version: "0.4.0".into(),
                started_at: chrono::Utc::now(),
                binary_path: Some("/opt/homebrew/bin/ctxd".into()),
            }),
            client_configs: vec![ClientConfigSnapshot {
                client: "claude-desktop".into(),
                path: "/tmp/x".into(),
                existed: true,
                contents: Some(b"{}".to_vec()),
            }],
            db_path: "/tmp/ctxd.db".into(),
        };
        let bytes = serde_json::to_vec(&snap).unwrap();
        let back: OnboardSnapshot = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back.version, 1);
        assert_eq!(back.running_daemon.as_ref().unwrap().pid, 1234);
        assert_eq!(back.client_configs.len(), 1);
    }
}
