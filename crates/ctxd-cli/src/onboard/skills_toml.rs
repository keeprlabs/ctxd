//! `skills.toml` — the daemon's adapter manifest.
//!
//! Lives at `<config_dir>/ctxd/skills.toml`. The serve loop reads
//! it at startup and spawns each enabled adapter as an in-process
//! tokio task. `ctxd onboard --only configure-adapters` writes
//! entries based on the user's CLI flags (`--fs=<paths>`, etc).
//!
//! ## Schema (v1)
//!
//! ```toml
//! [fs]
//! enabled = true
//! paths = ["/Users/me/Documents/notes", "/Users/me/Desktop"]
//! poll_interval_s = 60          # ignored for fs (event-driven)
//!
//! [gmail]
//! enabled = false               # off until phase 3B Gmail OAuth lands
//! token_file = "<config_dir>/ctxd/adapter-tokens/gmail.bk"
//! poll_interval_s = 60
//!
//! [github]
//! enabled = false
//! token_file = "<config_dir>/ctxd/adapter-tokens/github.bk"
//! poll_interval_s = 300
//! ```
//!
//! Atomic write-temp-then-rename so the file is never half-written.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::onboard::paths;

/// Top-level `skills.toml` document.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillsToml {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fs: Option<FsSkill>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gmail: Option<GmailSkill>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github: Option<GithubSkill>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FsSkill {
    pub enabled: bool,
    /// Absolute paths to watch. Empty = no watching.
    #[serde(default)]
    pub paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GmailSkill {
    pub enabled: bool,
    /// Path to the encrypted refresh-token blob (mode 0600).
    pub token_file: PathBuf,
    #[serde(default = "default_poll_60")]
    pub poll_interval_s: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GithubSkill {
    pub enabled: bool,
    /// Path to the PAT blob (mode 0600).
    pub token_file: PathBuf,
    #[serde(default = "default_poll_300")]
    pub poll_interval_s: u32,
}

fn default_poll_60() -> u32 {
    60
}

fn default_poll_300() -> u32 {
    300
}

/// Canonical path to the manifest.
pub fn skills_toml_path() -> Result<PathBuf> {
    Ok(paths::config_dir()?.join("skills.toml"))
}

/// Read the manifest. Returns `Default` (everything disabled) if
/// the file doesn't exist.
pub fn read() -> Result<SkillsToml> {
    let path = skills_toml_path()?;
    match std::fs::read_to_string(&path) {
        Ok(s) => toml::from_str(&s).with_context(|| format!("parse {path:?}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(SkillsToml::default()),
        Err(e) => Err(anyhow::Error::new(e).context(format!("read {path:?}"))),
    }
}

/// Read from a specific path (used by `serve.rs` so it can take the
/// `<config_dir>` resolution from the daemon's data_dir-aware path).
pub fn read_at(path: &Path) -> Result<SkillsToml> {
    match std::fs::read_to_string(path) {
        Ok(s) => toml::from_str(&s).with_context(|| format!("parse {path:?}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(SkillsToml::default()),
        Err(e) => Err(anyhow::Error::new(e).context(format!("read {path:?}"))),
    }
}

/// Atomic write of the manifest.
pub fn write(manifest: &SkillsToml) -> Result<PathBuf> {
    let path = skills_toml_path()?;
    write_at(&path, manifest)?;
    Ok(path)
}

pub fn write_at(path: &Path, manifest: &SkillsToml) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("path {path:?} has no parent dir"))?;
    std::fs::create_dir_all(parent).with_context(|| format!("create_dir_all {parent:?}"))?;
    let tmp = parent.join(format!(".skills-{}.toml.tmp", std::process::id()));
    let body = toml::to_string_pretty(manifest).context("serialize skills.toml")?;
    std::fs::write(&tmp, &body).with_context(|| format!("write {tmp:?}"))?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename {tmp:?} -> {path:?}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_toml() {
        let m = SkillsToml {
            fs: Some(FsSkill {
                enabled: true,
                paths: vec!["/tmp/notes".into()],
            }),
            gmail: None,
            github: None,
        };
        let s = toml::to_string_pretty(&m).unwrap();
        assert!(s.contains("[fs]"));
        assert!(s.contains("enabled = true"));
        let back: SkillsToml = toml::from_str(&s).unwrap();
        assert!(back.fs.is_some());
        assert_eq!(back.fs.unwrap().paths.len(), 1);
    }

    #[test]
    fn empty_manifest_is_default() {
        let m = SkillsToml::default();
        let s = toml::to_string_pretty(&m).unwrap();
        // Fields with #[serde(skip_serializing_if = "Option::is_none")]
        // produce no headers when absent.
        assert!(!s.contains("[fs]"));
        assert!(!s.contains("[gmail]"));
        let back: SkillsToml = toml::from_str(&s).unwrap();
        assert!(back.fs.is_none());
    }

    #[test]
    fn read_at_returns_default_on_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("nope.toml");
        let m = read_at(&p).unwrap();
        assert!(m.fs.is_none());
    }

    #[test]
    fn write_at_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("skills.toml");
        let m = SkillsToml {
            fs: Some(FsSkill {
                enabled: true,
                paths: vec!["/a".into(), "/b".into()],
            }),
            gmail: None,
            github: None,
        };
        write_at(&p, &m).unwrap();
        let back = read_at(&p).unwrap();
        let fs = back.fs.unwrap();
        assert!(fs.enabled);
        assert_eq!(fs.paths.len(), 2);
    }
}
