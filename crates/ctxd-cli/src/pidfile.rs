//! Daemon pidfile lock + admin URL beacon.
//!
//! A pidfile lives next to the SQLite DB at `<db_path>.pid` (e.g.
//! `~/Library/Application Support/ctxd/ctxd.db.pid`). It serves three
//! purposes:
//!
//! 1. **Detect "ctxd is already running"** before bind, so a duplicate
//!    daemon exits with a friendly message instead of crashing inside
//!    `bind()` with EADDRINUSE further down the startup path.
//! 2. **Publish the admin URL** so external callers (the skill, `ctxd
//!    onboard`, `ctxd offboard`, future MCP-stdio proxies) can find
//!    the running daemon without scanning for ports.
//! 3. **Identify the daemon's binary version** so callers can detect
//!    "you reinstalled ctxd but the old daemon is still running".
//!
//! The pidfile is informational, not exclusive — the OS's TCP
//! bind-port lock provides actual mutual exclusion. Stale pidfiles
//! (process dead, port unbound) are detected and overwritten.
//!
//! ## Format
//!
//! Pretty-printed JSON. Stable across versions per the [`PidFile`]
//! struct's serde shape. Tools may parse it.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Contents of the pidfile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PidFile {
    /// PID of the daemon process.
    pub pid: u32,
    /// HTTP admin bind, e.g. `"127.0.0.1:7777"`.
    pub admin_bind: String,
    /// Wire-protocol bind, if the daemon started one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wire_bind: Option<String>,
    /// Daemon binary version (matches `cargo pkg version`).
    pub version: String,
    /// RFC 3339 timestamp of when this pidfile was written.
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// Absolute path to the SQLite DB this daemon is opened against.
    pub db_path: String,
}

/// Where to put the pidfile for a daemon backed by `db_path`.
///
/// The path is `<db_path>.pid`. Multiple daemons against different
/// DBs land in different pidfiles; multiple daemons against the same
/// DB collide on the same pidfile (which is exactly what we want).
pub fn pidfile_path(db_path: &Path) -> PathBuf {
    let mut p = db_path.to_path_buf();
    let new_name = match p.file_name() {
        Some(n) => {
            let mut s = n.to_os_string();
            s.push(".pid");
            s
        }
        None => "ctxd.db.pid".into(),
    };
    p.set_file_name(new_name);
    p
}

/// Read the pidfile if present.
pub fn read(db_path: &Path) -> Option<PidFile> {
    let bytes = std::fs::read(pidfile_path(db_path)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Atomically write the pidfile (write-temp-then-rename).
pub fn write(db_path: &Path, contents: &PidFile) -> Result<()> {
    let final_path = pidfile_path(db_path);
    let parent = final_path
        .parent()
        .context("pidfile target has no parent dir")?;
    std::fs::create_dir_all(parent).with_context(|| format!("create dir {parent:?}"))?;
    let tmp = parent.join(format!(".ctxd-pidfile-{}.tmp", std::process::id()));
    let bytes = serde_json::to_vec_pretty(contents).context("serialize pidfile")?;
    std::fs::write(&tmp, &bytes).with_context(|| format!("write {tmp:?}"))?;
    std::fs::rename(&tmp, &final_path)
        .with_context(|| format!("rename {tmp:?} -> {final_path:?}"))?;
    Ok(())
}

/// Remove the pidfile, but only if it still names this process. This
/// guards against the (rare) case where a sibling daemon started
/// after we wrote our pidfile — we don't want to remove its file.
pub fn remove_if_ours(db_path: &Path) {
    if let Some(pf) = read(db_path) {
        if pf.pid == std::process::id() {
            let _ = std::fs::remove_file(pidfile_path(db_path));
        }
    }
}

/// Kernel-level liveness check. Returns `true` if signal 0 can be
/// delivered to `pid` (i.e. process exists and we have permission).
pub fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: kill(pid, 0) is a pure liveness probe; it does not
        // affect the target process state. We only read the return
        // value.
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        // Windows/other: we can't cheaply probe by PID. Treat as
        // alive and force the caller to fall back to /health probing.
        let _ = pid;
        true
    }
}

/// Probe `<admin_bind>/health` with a short timeout. Returns `true`
/// if the daemon answered with a 2xx status.
///
/// Implemented as raw HTTP/1.1 over TCP (no `reqwest` dep) because
/// this lives on the hot path of every `ctxd serve` startup and we
/// don't want to pull a TLS-capable client in for a loopback probe.
///
/// `Host:` is set to `admin_bind` itself rather than the literal
/// `localhost`, because ctxd's HTTP host-check middleware
/// (`allowed_hosts_for_bind`) gates on the actual bind — bare
/// `localhost` would be rejected with a 421 on any non-default port.
///
/// We deliberately do NOT half-close the write side after sending
/// the request: hyper's HTTP/1.1 server reads to message-complete
/// (Content-Length: 0 implicit for GET) and only then writes the
/// response. A premature `shutdown(SHUT_WR)` from us while hyper is
/// mid-state-machine causes it to drop the connection without
/// responding on some macOS/Linux kernels — `Connection: close` in
/// our request is enough to make the server close after writing.
pub async fn health_probe(admin_bind: &str) -> bool {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let connect = tokio::time::timeout(
        Duration::from_millis(800),
        tokio::net::TcpStream::connect(admin_bind),
    )
    .await;
    let mut stream = match connect {
        Ok(Ok(s)) => s,
        _ => return false,
    };
    let req = format!("GET /health HTTP/1.1\r\nHost: {admin_bind}\r\nConnection: close\r\n\r\n");
    if stream.write_all(req.as_bytes()).await.is_err() {
        return false;
    }
    if stream.flush().await.is_err() {
        return false;
    }
    // axum/hyper may flush the status line and headers in separate
    // chunks; loop until we have enough to read the status line or
    // the connection closes.
    let mut buf = [0u8; 256];
    let mut head = Vec::with_capacity(64);
    let deadline = std::time::Instant::now() + Duration::from_millis(800);
    while head.len() < 16 {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, stream.read(&mut buf)).await {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => head.extend_from_slice(&buf[..n]),
            _ => break,
        }
    }
    let head_str = std::str::from_utf8(&head).unwrap_or("");
    head_str.starts_with("HTTP/1.1 2") || head_str.starts_with("HTTP/1.0 2")
}

/// Daemon state inferred from the pidfile + a liveness probe.
#[derive(Debug)]
pub enum DaemonState {
    /// No pidfile, or the named PID is not alive. Free to start.
    NotRunning,
    /// Pidfile present, PID alive, `/health` responsive.
    Running(PidFile),
    /// Pidfile present, PID alive, `/health` did not respond. Likely
    /// mid-startup or wedged.
    Unresponsive(PidFile),
    /// Pidfile present but PID is dead. Stale; safe to overwrite.
    Stale(PidFile),
}

impl DaemonState {
    /// Pidfile contents if any state held one.
    pub fn pidfile(&self) -> Option<&PidFile> {
        match self {
            DaemonState::NotRunning => None,
            DaemonState::Running(pf) | DaemonState::Unresponsive(pf) | DaemonState::Stale(pf) => {
                Some(pf)
            }
        }
    }

    /// `true` if a fresh `ctxd serve` should refuse to start because
    /// another daemon is already serving the same DB.
    pub fn blocks_startup(&self) -> bool {
        matches!(self, DaemonState::Running(_) | DaemonState::Unresponsive(_))
    }
}

/// Inspect the pidfile alongside `db_path` and classify the running
/// daemon (or absence thereof).
pub async fn detect(db_path: &Path) -> DaemonState {
    let pf = match read(db_path) {
        Some(p) => p,
        None => return DaemonState::NotRunning,
    };
    if !pid_alive(pf.pid) {
        return DaemonState::Stale(pf);
    }
    if health_probe(&pf.admin_bind).await {
        DaemonState::Running(pf)
    } else {
        DaemonState::Unresponsive(pf)
    }
}

/// RAII guard that writes a pidfile on construction and removes it on
/// drop (only if the file still names this process). Use this to wrap
/// the daemon's lifetime so a panic or normal exit cleans up.
pub struct PidfileGuard {
    db_path: PathBuf,
}

impl PidfileGuard {
    /// Write the pidfile and return a guard that will remove it on drop.
    pub fn install(db_path: &Path, contents: &PidFile) -> Result<Self> {
        write(db_path, contents)?;
        Ok(Self {
            db_path: db_path.to_path_buf(),
        })
    }
}

impl Drop for PidfileGuard {
    fn drop(&mut self) {
        remove_if_ours(&self.db_path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pidfile_path_appends_pid_suffix() {
        let p = pidfile_path(Path::new("/var/lib/ctxd/ctxd.db"));
        assert_eq!(p, Path::new("/var/lib/ctxd/ctxd.db.pid"));
    }

    #[test]
    fn pidfile_path_handles_relative_db() {
        let p = pidfile_path(Path::new("ctxd.db"));
        assert_eq!(p, Path::new("ctxd.db.pid"));
    }

    #[test]
    fn pidfile_path_handles_no_filename() {
        // Edge case: `Path::new("/")` has no file name.
        let p = pidfile_path(Path::new("/"));
        assert_eq!(p.file_name().unwrap(), "ctxd.db.pid");
    }

    #[test]
    fn write_then_read_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("ctxd.db");
        let pf = PidFile {
            pid: 12345,
            admin_bind: "127.0.0.1:7777".to_string(),
            wire_bind: Some("127.0.0.1:7778".to_string()),
            version: "0.4.0".to_string(),
            started_at: chrono::Utc::now(),
            db_path: db.to_string_lossy().into_owned(),
        };
        write(&db, &pf).unwrap();
        let back = read(&db).expect("pidfile present");
        assert_eq!(back.pid, 12345);
        assert_eq!(back.admin_bind, "127.0.0.1:7777");
        assert_eq!(back.wire_bind.as_deref(), Some("127.0.0.1:7778"));
    }

    #[test]
    fn remove_if_ours_does_nothing_when_not_ours() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("ctxd.db");
        let pf = PidFile {
            pid: 999_999, // very unlikely to match this test process
            admin_bind: "127.0.0.1:7777".to_string(),
            wire_bind: None,
            version: "0.4.0".to_string(),
            started_at: chrono::Utc::now(),
            db_path: db.to_string_lossy().into_owned(),
        };
        write(&db, &pf).unwrap();
        remove_if_ours(&db);
        // The file should still exist because PID didn't match.
        assert!(read(&db).is_some(), "pidfile should NOT have been removed");
    }

    #[test]
    fn pidfile_guard_removes_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("ctxd.db");
        let pf = PidFile {
            pid: std::process::id(),
            admin_bind: "127.0.0.1:0".to_string(),
            wire_bind: None,
            version: "0.4.0".to_string(),
            started_at: chrono::Utc::now(),
            db_path: db.to_string_lossy().into_owned(),
        };
        {
            let _g = PidfileGuard::install(&db, &pf).unwrap();
            assert!(
                read(&db).is_some(),
                "pidfile should be present inside guard scope"
            );
        }
        assert!(
            read(&db).is_none(),
            "pidfile should have been removed by guard's Drop"
        );
    }

    #[test]
    fn pid_alive_for_self_is_true() {
        assert!(pid_alive(std::process::id()));
    }

    #[test]
    fn pid_alive_for_invented_high_pid_is_false() {
        // PID 999_999_999 is virtually guaranteed not to exist.
        assert!(!pid_alive(999_999_999));
    }

    #[tokio::test]
    async fn health_probe_returns_false_for_unreachable() {
        // Pick a port nothing should be listening on. 127.0.0.1:1 is
        // a privileged port, never bound by a normal process; the
        // connect() will fail-fast with ECONNREFUSED.
        assert!(!health_probe("127.0.0.1:1").await);
    }

    #[tokio::test]
    async fn detect_no_pidfile_is_not_running() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("ctxd.db");
        let state = detect(&db).await;
        assert!(matches!(state, DaemonState::NotRunning));
        assert!(!state.blocks_startup());
    }

    #[tokio::test]
    async fn detect_stale_pidfile_with_dead_pid() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("ctxd.db");
        let pf = PidFile {
            pid: 999_999_999,
            admin_bind: "127.0.0.1:0".to_string(),
            wire_bind: None,
            version: "0.4.0".to_string(),
            started_at: chrono::Utc::now(),
            db_path: db.to_string_lossy().into_owned(),
        };
        write(&db, &pf).unwrap();
        let state = detect(&db).await;
        assert!(matches!(state, DaemonState::Stale(_)));
        assert!(!state.blocks_startup());
    }

    #[tokio::test]
    async fn detect_unresponsive_when_pid_alive_but_health_fails() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("ctxd.db");
        let pf = PidFile {
            // Our own PID is definitely alive, but we don't have a
            // /health endpoint listening here, so the probe fails.
            pid: std::process::id(),
            admin_bind: "127.0.0.1:1".to_string(),
            wire_bind: None,
            version: "0.4.0".to_string(),
            started_at: chrono::Utc::now(),
            db_path: db.to_string_lossy().into_owned(),
        };
        write(&db, &pf).unwrap();
        let state = detect(&db).await;
        assert!(
            matches!(state, DaemonState::Unresponsive(_)),
            "expected Unresponsive, got {state:?}"
        );
        assert!(state.blocks_startup());
    }
}
