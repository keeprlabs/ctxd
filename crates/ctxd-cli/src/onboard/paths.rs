//! Cross-platform path resolution for onboard / offboard.
//!
//! Wraps the `directories` crate so call sites get one canonical
//! answer for "where do ctxd's data files live, where does the
//! user's MCP-client config live, where do logs go" — rather than
//! re-deriving each platform's conventions in three places.
//!
//! Conventions (matching ctxd's existing brew install path):
//!
//! | Purpose         | macOS                                    | Linux                                  | Windows                            |
//! |-----------------|------------------------------------------|----------------------------------------|------------------------------------|
//! | data            | `~/Library/Application Support/ctxd/`    | `$XDG_DATA_HOME/ctxd` (`~/.local/share/ctxd`) | `%APPDATA%/ctxd/data/`         |
//! | config          | `~/Library/Application Support/ctxd/`    | `$XDG_CONFIG_HOME/ctxd` (`~/.config/ctxd`)    | `%APPDATA%/ctxd/config/`       |
//! | logs            | `~/Library/Logs/ctxd/`                   | `$XDG_STATE_HOME/ctxd/logs` (`~/.local/state/ctxd/logs`) | `%APPDATA%/ctxd/logs/` |
//! | claude desktop  | `~/Library/Application Support/Claude/claude_desktop_config.json` | `~/.config/Claude/claude_desktop_config.json` | `%APPDATA%/Claude/claude_desktop_config.json` |
//! | claude code     | `~/.claude/settings.json`                | `~/.claude/settings.json`              | `%APPDATA%/Claude Code/settings.json` |
//!
//! All functions are pure — they compute paths and return them
//! without touching the filesystem. Callers `std::fs::create_dir_all`
//! when they need to materialise a directory.

use anyhow::{Context, Result};
#[cfg(not(target_os = "macos"))]
use directories::ProjectDirs;
use std::path::PathBuf;

/// Reverse-DNS qualifier used for the macOS `ProjectDirs` lookup and
/// the launchd label. `dev.ctxd.daemon` matches the public domain
/// `ctxd.dev` per Apple's reverse-DNS convention.
pub const SERVICE_LABEL: &str = "dev.ctxd.daemon";

#[cfg(not(target_os = "macos"))]
fn project_dirs() -> Result<ProjectDirs> {
    ProjectDirs::from("dev", "ctxd", "ctxd")
        .context("could not resolve user directories — $HOME unset?")
}

/// Where ctxd stores the SQLite DB + HNSW index sidecars. Matches
/// the Homebrew install path on macOS so `brew install` and
/// `ctxd onboard` resolve to the same location.
///
/// `CTXD_DATA_DIR` overrides the default. Used by integration tests
/// to isolate state per test (each test sets a tempdir) and by
/// operators who want to point ctxd at a non-default location
/// without recompiling.
pub fn data_dir() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("CTXD_DATA_DIR") {
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    #[cfg(target_os = "macos")]
    {
        // Force `Application Support/ctxd` rather than
        // `Application Support/dev.ctxd.ctxd` (which is what
        // ProjectDirs::data_dir would return on macOS). Matches the
        // brew formula's canonical location.
        let home = std::env::var("HOME").context("$HOME not set")?;
        Ok(PathBuf::from(home).join("Library/Application Support/ctxd"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(project_dirs()?.data_dir().to_path_buf())
    }
}

/// Where ctxd stores user-facing configuration (`skills.toml`,
/// capability file pointers, snapshot manifests).
///
/// `CTXD_CONFIG_DIR` overrides the default. Used by integration
/// tests to isolate state per test (each test sets a tempdir).
pub fn config_dir() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("CTXD_CONFIG_DIR") {
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    #[cfg(target_os = "macos")]
    {
        // On macOS we co-locate config with data — Apple convention
        // is to use Application Support for both. Avoids the
        // `~/Library/Preferences` plist trap that's unfriendly to
        // hand-editing.
        data_dir()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(project_dirs()?.config_dir().to_path_buf())
    }
}

/// Where ctxd writes log files when running as a service (launchd /
/// systemd). For Linux+systemd the journal is the canonical sink;
/// these paths are still useful for the foreground `ctxd serve` mode
/// when redirected.
pub fn log_dir() -> Result<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").context("$HOME not set")?;
        Ok(PathBuf::from(home).join("Library/Logs/ctxd"))
    }
    #[cfg(target_os = "linux")]
    {
        Ok(project_dirs()?
            .state_dir()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| project_dirs().unwrap().data_dir().join("logs")))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Ok(project_dirs()?.data_dir().join("logs"))
    }
}

/// Default path for the SQLite DB. Used when the user does not
/// explicitly pass `--db`.
pub fn default_db_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("ctxd.db"))
}

/// Per-client capability file directory. Each minted token lives at
/// `<config_dir>/caps/<client>.bk` with mode `0600`.
pub fn caps_dir() -> Result<PathBuf> {
    Ok(config_dir()?.join("caps"))
}

/// Adapter token storage (Gmail refresh token, GitHub PAT, etc),
/// mode `0600`. Distinct from `caps_dir` because adapters may have
/// adapter-specific encrypted blobs (Gmail uses AES-256-GCM).
pub fn adapter_tokens_dir() -> Result<PathBuf> {
    Ok(config_dir()?.join("adapter-tokens"))
}

/// Pre-flight snapshot directory used by `onboard` to record the
/// system state before it mutates anything (so `offboard` can
/// restore).
pub fn snapshots_dir() -> Result<PathBuf> {
    Ok(data_dir()?.join("onboard-snapshots"))
}

/// Path to the Claude Desktop MCP config file. Returns the file even
/// if it doesn't exist yet — caller is responsible for creating it
/// on write or skipping on detect.
pub fn claude_desktop_config() -> Result<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").context("$HOME not set")?;
        Ok(PathBuf::from(home)
            .join("Library/Application Support/Claude/claude_desktop_config.json"))
    }
    #[cfg(target_os = "linux")]
    {
        let home = std::env::var("HOME").context("$HOME not set")?;
        Ok(PathBuf::from(home).join(".config/Claude/claude_desktop_config.json"))
    }
    #[cfg(target_os = "windows")]
    {
        let appdata = std::env::var("APPDATA").context("%APPDATA% not set")?;
        Ok(PathBuf::from(appdata).join("Claude/claude_desktop_config.json"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        anyhow::bail!("unsupported platform for Claude Desktop")
    }
}

/// Path to Claude Code's user-scope settings.json (the file that
/// holds `mcpServers` plus hook config).
pub fn claude_code_config() -> Result<PathBuf> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .context("$HOME / %USERPROFILE% not set")?;
    Ok(PathBuf::from(home).join(".claude/settings.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_resolve_under_home() {
        // Smoke: every getter returns a path that's at least one
        // component long. Don't assert exact prefixes — the test
        // host's HOME may be wherever.
        assert!(data_dir().unwrap().components().count() >= 1);
        assert!(config_dir().unwrap().components().count() >= 1);
        assert!(log_dir().unwrap().components().count() >= 1);
        assert!(default_db_path().unwrap().components().count() >= 2);
        assert!(caps_dir().unwrap().ends_with("caps"));
        assert!(adapter_tokens_dir().unwrap().ends_with("adapter-tokens"));
        assert!(snapshots_dir().unwrap().ends_with("onboard-snapshots"));
        assert!(claude_desktop_config()
            .unwrap()
            .ends_with("claude_desktop_config.json"));
        assert!(claude_code_config().unwrap().ends_with("settings.json"));
    }

    #[test]
    fn default_db_path_is_under_data_dir() {
        let db = default_db_path().unwrap();
        let data = data_dir().unwrap();
        assert!(db.starts_with(&data), "{db:?} should be under {data:?}");
        assert_eq!(db.file_name().unwrap(), "ctxd.db");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_data_dir_matches_homebrew_canonical() {
        // The brew install path is `~/Library/Application Support/ctxd/`
        // — onboarded daemons MUST land in the same place so a brew
        // user's existing DB is found.
        let p = data_dir().unwrap();
        assert!(p.ends_with("Library/Application Support/ctxd"), "got {p:?}");
    }

    #[test]
    fn service_label_uses_reverse_dns() {
        // launchd plist filenames use this label. Apple convention is
        // reverse-DNS of the controlling domain. ctxd.dev → dev.ctxd.
        assert!(SERVICE_LABEL.starts_with("dev.ctxd"));
    }
}
