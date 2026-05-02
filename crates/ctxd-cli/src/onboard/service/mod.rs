//! Cross-platform service install / start / stop for `ctxd serve`.
//!
//! [`ServiceBackend`] is the trait every platform implements;
//! [`detect_backend`] returns the right impl for the current OS.
//!
//! ## Idempotency
//!
//! Every method is idempotent. Re-installing with the same
//! [`ServiceSpec`] reports `Unchanged`; starting an already-running
//! service is a no-op; uninstalling on a clean system reports
//! `NotInstalled` without error. This is essential for `ctxd onboard
//! --only service-install` and the post-doctor remediation paths
//! that may call install multiple times.
//!
//! ## What lives where
//!
//! | Platform | File path                                         | Tool          |
//! |----------|---------------------------------------------------|---------------|
//! | macOS    | `~/Library/LaunchAgents/dev.ctxd.daemon.plist`    | `launchctl`   |
//! | Linux    | `~/.config/systemd/user/ctxd.service`             | `systemctl --user` |
//! | Windows  | (not yet — phase 0.5)                             | —             |
//!
//! Both unit/plist files use atomic write-temp-then-rename so a
//! crash mid-install can never leave a half-written file.
//!
//! ## What this module does NOT do
//!
//! Mint capability tokens, write client config files, decide which
//! adapters to enable — those are higher-level onboard concerns.
//! This layer only manages the service unit + lifecycle.

use anyhow::{Context, Result};
use std::path::PathBuf;

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
pub mod null;

/// Configuration for installing a ctxd service.
#[derive(Debug, Clone)]
pub struct ServiceSpec {
    /// Absolute path to the `ctxd` binary the service will execute.
    /// Typically `std::env::current_exe()` so `cargo run`-style dev
    /// installs point at the dev binary, and `brew install` users get
    /// `/opt/homebrew/bin/ctxd`.
    pub binary: PathBuf,

    /// Arguments to pass after `serve`. The launcher always invokes
    /// `<binary> serve <args...>`. Typically `["--db", "<db_path>",
    /// "--bind", "127.0.0.1:7777", ...]`.
    pub args: Vec<String>,

    /// `true` to start the service automatically when the user logs
    /// in (launchd `RunAtLoad`, systemd `WantedBy=default.target`
    /// link). Default off — opt-in via `ctxd onboard --at-login`.
    pub at_login: bool,

    /// Working directory for the spawned process. Typically
    /// `paths::data_dir()`. Used so relative `--db ctxd.db` paths
    /// resolve consistently.
    pub working_dir: PathBuf,

    /// Where to redirect stdout/stderr on macOS. Ignored on Linux
    /// (systemd-journald captures them).
    pub log_dir: PathBuf,

    /// Service identifier. macOS uses this as the launchd label
    /// (`dev.ctxd.daemon`); Linux uses `<label>.service` as the unit
    /// filename (after stripping the reverse-DNS prefix to keep
    /// `systemctl status` readable — see [`linux::Systemd::unit_name`]).
    pub label: String,
}

/// Outcome of [`ServiceBackend::install`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallReport {
    /// What changed on disk (or didn't).
    pub action: InstallAction,
    /// Path to the unit/plist file we wrote (or would write).
    pub unit_path: PathBuf,
}

/// Idempotency classification for a service install.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallAction {
    /// Unit/plist file was written for the first time.
    Created,
    /// Unit/plist already existed but had different contents (e.g.
    /// the user reinstalled ctxd into a different prefix).
    Updated,
    /// Unit/plist file already matches the requested spec exactly.
    Unchanged,
}

/// Coarse-grained service state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceStatus {
    /// No unit/plist on disk.
    NotInstalled,
    /// Unit/plist present but the service is not running.
    Stopped,
    /// Service is running. `pid` is best-effort; some launchctl /
    /// systemctl combinations don't surface it.
    Running { pid: Option<u32> },
}

/// Cross-platform interface for managing the ctxd service.
pub trait ServiceBackend: Send + Sync {
    /// Install (or update) the service unit. Idempotent.
    fn install(&self, spec: &ServiceSpec) -> Result<InstallReport>;

    /// Start the service. No-op if already running.
    fn start(&self) -> Result<()>;

    /// Stop the service. No-op if already stopped or not installed.
    fn stop(&self) -> Result<()>;

    /// Stop and remove the service. Idempotent.
    fn uninstall(&self) -> Result<()>;

    /// Coarse-grained status. Cheap to call.
    fn status(&self) -> Result<ServiceStatus>;

    /// Path to the unit/plist file this backend manages. Useful for
    /// `ctxd doctor` and offboard-restore so callers don't have to
    /// re-derive it.
    fn unit_path(&self) -> PathBuf;

    /// Human-readable platform name (e.g. `"launchd"`, `"systemd-user"`).
    fn name(&self) -> &'static str;
}

/// Return the appropriate [`ServiceBackend`] for the current OS.
///
/// On macOS returns a [`macos::Launchd`]; on Linux,
/// [`linux::Systemd`]; on every other platform a [`null::NullBackend`]
/// that fails fast with a clear "not yet supported" error on every
/// call. Callers that want to detect lack of support without
/// triggering an error use [`is_supported`].
pub fn detect_backend(label: impl Into<String>) -> Result<Box<dyn ServiceBackend>> {
    let label = label.into();
    #[cfg(target_os = "macos")]
    let backend: Box<dyn ServiceBackend> = Box::new(macos::Launchd::new(label)?);
    #[cfg(target_os = "linux")]
    let backend: Box<dyn ServiceBackend> = Box::new(linux::Systemd::new(label)?);
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let backend: Box<dyn ServiceBackend> = Box::new(null::NullBackend::new(label));
    Ok(backend)
}

/// `true` if the current platform has a real (non-null) backend.
pub fn is_supported() -> bool {
    cfg!(any(target_os = "macos", target_os = "linux"))
}

/// Atomic write helper used by both backends: write to a sibling
/// `.tmp` file then rename. Crash-safe: a partial write is left in
/// the `.tmp` file and never visible at the final path.
pub(crate) fn atomic_write(path: &std::path::Path, contents: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("atomic_write: path {path:?} has no parent"))?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("atomic_write: create_dir_all {parent:?}"))?;
    let tmp = parent.join(format!(
        ".{}.tmp.{}",
        path.file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unit".to_string()),
        std::process::id()
    ));
    std::fs::write(&tmp, contents).with_context(|| format!("atomic_write: write {tmp:?}"))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("atomic_write: rename {tmp:?} -> {path:?}"))?;
    Ok(())
}

/// Read the file if present. Returns `None` on `NotFound`.
pub(crate) fn read_if_exists(path: &std::path::Path) -> Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(anyhow::Error::new(e).context(format!("read_if_exists: {path:?}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a/b/c/file.txt");
        atomic_write(&nested, b"hello").unwrap();
        assert_eq!(std::fs::read(&nested).unwrap(), b"hello");
    }

    #[test]
    fn atomic_write_replaces_existing() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.txt");
        atomic_write(&p, b"v1").unwrap();
        atomic_write(&p, b"v2").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"v2");
    }

    #[test]
    fn read_if_exists_returns_none_on_missing() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope");
        assert!(read_if_exists(&missing).unwrap().is_none());
    }

    #[test]
    fn read_if_exists_returns_contents() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("there");
        std::fs::write(&p, b"hi").unwrap();
        assert_eq!(read_if_exists(&p).unwrap(), Some(b"hi".to_vec()));
    }

    #[test]
    fn detect_backend_returns_real_impl_on_supported_platforms() {
        let b = detect_backend("dev.ctxd.daemon").unwrap();
        let name = b.name();
        if cfg!(target_os = "macos") {
            assert_eq!(name, "launchd");
        } else if cfg!(target_os = "linux") {
            assert_eq!(name, "systemd-user");
        } else {
            assert_eq!(name, "null");
        }
    }

    #[test]
    fn is_supported_matches_cfg() {
        let expected = cfg!(any(target_os = "macos", target_os = "linux"));
        assert_eq!(is_supported(), expected);
    }
}
