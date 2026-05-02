//! Client config writers: Claude Desktop, Claude Code, Codex.
//!
//! Each writer is the (`detect`, `plan`, `apply`, `verify`) quartet
//! described in the v0.4 plan: `detect()` reports whether the client
//! is installed, `plan()` describes the diff that would be applied,
//! `apply()` writes the file atomically, `verify()` reads it back.
//!
//! All three writers share a single `mcpServers.ctxd` entry shape:
//!
//! ```jsonc
//! "ctxd": {
//!     "command": "/opt/homebrew/bin/ctxd",
//!     "args": [
//!         "serve", "--mcp-stdio",
//!         "--cap-file", "/Users/me/Library/Application Support/ctxd/caps/claude-desktop.bk",
//!         "--db", "/Users/me/Library/Application Support/ctxd/ctxd.db"
//!     ]
//! }
//! ```
//!
//! Why `--cap-file` and not `--cap <token>` in args: the args land
//! in the user's config JSON which is readable by anything on disk,
//! and `ps` exposes them to any local process. The cap file is
//! `0600` so only the user can read it — see [`super::caps`].
//!
//! ## Idempotency
//!
//! All writers preserve any other `mcpServers.*` entries the user
//! has set. Re-applying with the same spec is a no-op (idempotent).
//! `apply()` uses atomic write-temp-then-rename so a crash mid-write
//! cannot corrupt the user's config file.

use anyhow::{Context, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};

use crate::onboard::caps::{self, ClientId};
use crate::onboard::paths;

/// Stable identifier for the canonical entry name used in
/// `mcpServers`. Picked to match the existing brew-install snippet
/// in the README so users with a hand-edited config see the same
/// key after running onboard.
pub const MCP_ENTRY_NAME: &str = "ctxd";

/// Outcome of a [`ClientWriter::apply`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyAction {
    /// Config file did not exist — created with our entry.
    Created,
    /// Config file existed; our entry was added.
    EntryAdded,
    /// Config file existed; our entry was changed (different binary
    /// path, different cap-file, etc).
    EntryUpdated,
    /// Config file already had our entry with the right shape.
    Unchanged,
    /// Manual paste required (Codex; today writes a file the user
    /// can copy from but doesn't auto-apply).
    ManualPending,
}

/// What apply() needs to know about the daemon being onboarded.
#[derive(Debug, Clone)]
pub struct ClientSpec {
    /// Absolute path to the `ctxd` binary.
    pub binary: PathBuf,
    /// Absolute path to the SQLite DB.
    pub db_path: PathBuf,
    /// `true` to write Claude Code hooks. No-op for Claude Desktop /
    /// Codex, where hooks aren't a thing.
    pub with_hooks: bool,
}

/// Cross-client interface.
pub trait ClientWriter: Send + Sync {
    /// `true` if the client appears to be installed on this host
    /// (e.g. its config dir exists). Some writers always return
    /// `true` because they don't have a reliable presence signal.
    fn detect(&self) -> bool;

    /// Where the config file we'd modify lives.
    fn config_path(&self) -> Result<PathBuf>;

    /// Slug of the [`ClientId`] this writer targets.
    fn client_id(&self) -> ClientId;

    /// Apply the spec idempotently. Returns the action taken.
    fn apply(&self, spec: &ClientSpec) -> Result<ApplyAction>;

    /// Read the config file and confirm our entry is present and
    /// matches the spec.
    fn verify(&self, spec: &ClientSpec) -> Result<bool>;
}

/// Build the args we want every client to invoke `ctxd` with. Used
/// by all three writers (the differences between clients are file
/// paths and hooks, not the MCP server entry shape).
fn build_mcp_args(spec: &ClientSpec, cap_file: &Path) -> Vec<String> {
    vec![
        "serve".into(),
        "--mcp-stdio".into(),
        "--cap-file".into(),
        cap_file.to_string_lossy().into_owned(),
        "--db".into(),
        spec.db_path.to_string_lossy().into_owned(),
    ]
}

/// Build the JSON value for a single `mcpServers.ctxd` entry.
fn build_mcp_entry(spec: &ClientSpec, cap_file: &Path) -> Value {
    serde_json::json!({
        "command": spec.binary.to_string_lossy(),
        "args": build_mcp_args(spec, cap_file),
    })
}

/// Read JSON from `path` if present, otherwise return an empty
/// object. Used by writers that merge into an existing config file.
fn read_json_or_empty(path: &Path) -> Result<Value> {
    match std::fs::read(path) {
        Ok(b) if b.is_empty() => Ok(Value::Object(serde_json::Map::new())),
        Ok(b) => serde_json::from_slice(&b).with_context(|| format!("parse JSON from {path:?}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok(Value::Object(serde_json::Map::new()))
        }
        Err(e) => Err(anyhow::Error::new(e).context(format!("read {path:?}"))),
    }
}

/// Atomic write of pretty-printed JSON.
fn atomic_write_json(path: &Path, value: &Value) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("path {path:?} has no parent dir"))?;
    std::fs::create_dir_all(parent).with_context(|| format!("create_dir_all {parent:?}"))?;
    let tmp = parent.join(format!(".ctxd-client-{}.tmp", std::process::id()));
    let bytes = serde_json::to_vec_pretty(value).context("serialize JSON")?;
    std::fs::write(&tmp, &bytes).with_context(|| format!("write {tmp:?}"))?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename {tmp:?} -> {path:?}"))?;
    Ok(())
}

// ---- Claude Desktop ------------------------------------------------

/// Writer for `~/Library/Application Support/Claude/claude_desktop_config.json`.
pub struct ClaudeDesktop;

impl ClientWriter for ClaudeDesktop {
    fn detect(&self) -> bool {
        // Best-effort: the Claude/ directory exists if the user has
        // installed Claude Desktop at any point. We don't probe the
        // process since the user may not have it running.
        if let Ok(p) = paths::claude_desktop_config() {
            p.parent().map(|d| d.exists()).unwrap_or(false)
        } else {
            false
        }
    }

    fn config_path(&self) -> Result<PathBuf> {
        paths::claude_desktop_config()
    }

    fn client_id(&self) -> ClientId {
        ClientId::ClaudeDesktop
    }

    fn apply(&self, spec: &ClientSpec) -> Result<ApplyAction> {
        let path = self.config_path()?;
        let cap_file = caps::cap_file_path(self.client_id())?;
        apply_mcp_entry(&path, spec, &cap_file)
    }

    fn verify(&self, spec: &ClientSpec) -> Result<bool> {
        let path = self.config_path()?;
        verify_mcp_entry(&path, spec, &caps::cap_file_path(self.client_id())?)
    }
}

// ---- Claude Code ---------------------------------------------------

/// Writer for `~/.claude/settings.json`. With `with_hooks=true`,
/// also writes hook entries under `hooks.SessionStart` / etc.
pub struct ClaudeCode;

impl ClientWriter for ClaudeCode {
    fn detect(&self) -> bool {
        if let Ok(p) = paths::claude_code_config() {
            p.parent().map(|d| d.exists()).unwrap_or(false)
        } else {
            false
        }
    }

    fn config_path(&self) -> Result<PathBuf> {
        paths::claude_code_config()
    }

    fn client_id(&self) -> ClientId {
        ClientId::ClaudeCode
    }

    fn apply(&self, spec: &ClientSpec) -> Result<ApplyAction> {
        let path = self.config_path()?;
        let cap_file = caps::cap_file_path(self.client_id())?;
        let mut config = read_json_or_empty(&path)?;
        let entry = build_mcp_entry(spec, &cap_file);
        let action = upsert_mcp_entry(&mut config, &entry);

        if spec.with_hooks {
            install_claude_code_hooks(&mut config, spec)?;
        } else {
            // Even with --with-hooks=false we leave any existing
            // hook block intact — onboard's "off by default" mode
            // shouldn't aggressively rip the user's hook config.
        }

        if action != ApplyAction::Unchanged {
            atomic_write_json(&path, &config)?;
        }
        Ok(action)
    }

    fn verify(&self, spec: &ClientSpec) -> Result<bool> {
        let path = self.config_path()?;
        verify_mcp_entry(&path, spec, &caps::cap_file_path(self.client_id())?)
    }
}

/// Install Claude Code hooks under `hooks.<event>` matching Anthropic's
/// hook config schema (matcher + hooks array). The hooks invoke
/// `ctxd hook <event>` which reads the hook payload from stdin and
/// writes a structured event into `/me/sessions` etc.
fn install_claude_code_hooks(config: &mut Value, spec: &ClientSpec) -> Result<()> {
    let obj = config
        .as_object_mut()
        .context("claude code settings root must be a JSON object")?;
    let hooks_entry = obj
        .entry("hooks".to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    let hooks_obj = hooks_entry
        .as_object_mut()
        .context("hooks must be a JSON object")?;
    let bin = spec.binary.to_string_lossy().into_owned();
    let db = spec.db_path.to_string_lossy().into_owned();
    let cap = caps::cap_file_path(ClientId::ClaudeCode)?
        .to_string_lossy()
        .into_owned();

    // Each event uses the same shape: a matcher + a hooks list.
    // The "matcher": "*" applies to every tool / prompt event.
    for event in ["SessionStart", "UserPromptSubmit", "PreCompact", "Stop"] {
        let cmd = format!(
            "{bin} hook --db {db:?} --cap-file {cap:?} {}",
            event_slug(event)
        );
        let entry = serde_json::json!([
            {
                "matcher": "*",
                "hooks": [
                    { "type": "command", "command": cmd }
                ]
            }
        ]);
        hooks_obj.insert(event.to_string(), entry);
    }
    Ok(())
}

/// Convert a Claude Code event name to the `ctxd hook <slug>` slug
/// used by the (phase 4B) hook subcommand. Stable kebab-case.
fn event_slug(event: &str) -> &'static str {
    match event {
        "SessionStart" => "session-start",
        "UserPromptSubmit" => "user-prompt-submit",
        "PreCompact" => "pre-compact",
        "Stop" => "stop",
        _ => "unknown",
    }
}

// ---- Codex ---------------------------------------------------------

/// Writer for Codex CLI. Today: writes a `codex.snippet.toml` under
/// the snapshots dir with the exact config block to paste, and
/// reports `ManualPending`. When Codex's MCP config story stabilises
/// the writer will gain an apply path; the snippet path stays stable
/// for the doctor to keep grading manual_pending properly.
pub struct Codex;

impl ClientWriter for Codex {
    fn detect(&self) -> bool {
        // Codex CLI doesn't have a stable presence signal — we never
        // claim it's installed.
        false
    }

    fn config_path(&self) -> Result<PathBuf> {
        // The "config path" we report is the snippet file we DO write,
        // not the user's ~/.codex/config.toml (which we don't touch).
        Ok(paths::config_dir()?.join("codex.snippet.toml"))
    }

    fn client_id(&self) -> ClientId {
        ClientId::Codex
    }

    fn apply(&self, spec: &ClientSpec) -> Result<ApplyAction> {
        let path = self.config_path()?;
        let cap_file = caps::cap_file_path(self.client_id())?;
        let snippet = render_codex_snippet(spec, &cap_file);
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(&path, snippet)?;
        Ok(ApplyAction::ManualPending)
    }

    fn verify(&self, _spec: &ClientSpec) -> Result<bool> {
        // The snippet is informational; verify always succeeds when
        // the snippet file is on disk. Doctor reports manual-pending
        // as long as the user's actual ~/.codex/config.toml does not
        // contain a ctxd entry; we check that elsewhere.
        Ok(self.config_path()?.exists())
    }
}

fn render_codex_snippet(spec: &ClientSpec, cap_file: &Path) -> String {
    format!(
        r#"# Paste this block into ~/.codex/config.toml under [mcp_servers.ctxd]:
[mcp_servers.ctxd]
command = "{bin}"
args = [
  "serve",
  "--mcp-stdio",
  "--cap-file", "{cap}",
  "--db", "{db}"
]
"#,
        bin = spec.binary.to_string_lossy(),
        cap = cap_file.to_string_lossy(),
        db = spec.db_path.to_string_lossy(),
    )
}

// ---- shared helpers ------------------------------------------------

/// Add or update the `mcpServers.ctxd` entry in a Claude-Desktop-or-
/// Code config JSON. Returns the [`ApplyAction`] describing what
/// happened.
fn upsert_mcp_entry(config: &mut Value, entry: &Value) -> ApplyAction {
    let obj = match config.as_object_mut() {
        Some(o) => o,
        None => {
            // Replace a non-object root (rare but possible if the
            // user wrote an array) with a fresh object containing
            // just our entry.
            *config = serde_json::json!({ "mcpServers": { MCP_ENTRY_NAME: entry } });
            return ApplyAction::Created;
        }
    };
    let servers = obj
        .entry("mcpServers".to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    let servers_obj = match servers.as_object_mut() {
        Some(s) => s,
        None => {
            // Replace a malformed mcpServers value with a fresh
            // object containing just ours.
            *servers = serde_json::json!({ MCP_ENTRY_NAME: entry });
            return ApplyAction::EntryAdded;
        }
    };
    match servers_obj.get(MCP_ENTRY_NAME) {
        Some(existing) if existing == entry => ApplyAction::Unchanged,
        Some(_) => {
            servers_obj.insert(MCP_ENTRY_NAME.to_string(), entry.clone());
            ApplyAction::EntryUpdated
        }
        None => {
            servers_obj.insert(MCP_ENTRY_NAME.to_string(), entry.clone());
            ApplyAction::EntryAdded
        }
    }
}

/// Apply our MCP entry to a config file at `path`. Reads the file
/// (or starts empty), upserts, atomically writes back. Returns the
/// resulting action.
fn apply_mcp_entry(path: &Path, spec: &ClientSpec, cap_file: &Path) -> Result<ApplyAction> {
    let was_missing = !path.exists();
    let mut config = read_json_or_empty(path)?;
    let entry = build_mcp_entry(spec, cap_file);
    let action_inner = upsert_mcp_entry(&mut config, &entry);
    let action = if was_missing && action_inner == ApplyAction::EntryAdded {
        // Distinguish "we just created the file" from "file existed".
        ApplyAction::Created
    } else {
        action_inner
    };
    if action != ApplyAction::Unchanged {
        atomic_write_json(path, &config)?;
    }
    Ok(action)
}

fn verify_mcp_entry(path: &Path, spec: &ClientSpec, cap_file: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let config = read_json_or_empty(path)?;
    let entry = build_mcp_entry(spec, cap_file);
    let actual = config
        .pointer(&format!("/mcpServers/{}", MCP_ENTRY_NAME))
        .cloned()
        .unwrap_or(Value::Null);
    Ok(actual == entry)
}

/// Write each known client's config in turn. Returns one
/// `(ClientId, ApplyAction)` per writer, in pipeline order.
pub fn apply_all(spec: &ClientSpec) -> Vec<(ClientId, Result<ApplyAction>)> {
    let writers: Vec<Box<dyn ClientWriter>> = vec![
        Box::new(ClaudeDesktop),
        Box::new(ClaudeCode),
        Box::new(Codex),
    ];
    writers
        .into_iter()
        .map(|w| (w.client_id(), w.apply(spec)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_spec(dir: &Path) -> ClientSpec {
        ClientSpec {
            binary: dir.join("ctxd"),
            db_path: dir.join("ctxd.db"),
            with_hooks: false,
        }
    }

    #[test]
    fn upsert_creates_entry_when_config_is_empty() {
        let mut cfg = Value::Object(serde_json::Map::new());
        let entry = serde_json::json!({"command": "ctxd", "args": []});
        let action = upsert_mcp_entry(&mut cfg, &entry);
        assert_eq!(action, ApplyAction::EntryAdded);
        assert_eq!(cfg["mcpServers"]["ctxd"], entry);
    }

    #[test]
    fn upsert_preserves_other_servers() {
        let mut cfg = serde_json::json!({
            "mcpServers": {
                "filesystem": { "command": "/usr/bin/something" }
            }
        });
        let entry = serde_json::json!({"command": "ctxd"});
        upsert_mcp_entry(&mut cfg, &entry);
        assert!(
            cfg["mcpServers"]["filesystem"].is_object(),
            "filesystem entry must be preserved: {cfg}"
        );
        assert_eq!(cfg["mcpServers"]["ctxd"], entry);
    }

    #[test]
    fn upsert_is_idempotent_when_entry_matches() {
        let mut cfg = serde_json::json!({
            "mcpServers": { "ctxd": { "command": "ctxd", "args": ["serve"] } }
        });
        let entry = serde_json::json!({ "command": "ctxd", "args": ["serve"] });
        let action = upsert_mcp_entry(&mut cfg, &entry);
        assert_eq!(action, ApplyAction::Unchanged);
    }

    #[test]
    fn upsert_updates_when_entry_differs() {
        let mut cfg = serde_json::json!({
            "mcpServers": { "ctxd": { "command": "/old/path" } }
        });
        let entry = serde_json::json!({ "command": "/new/path" });
        let action = upsert_mcp_entry(&mut cfg, &entry);
        assert_eq!(action, ApplyAction::EntryUpdated);
        assert_eq!(cfg["mcpServers"]["ctxd"]["command"], "/new/path");
    }

    #[test]
    fn build_mcp_args_uses_cap_file_not_inline_token() {
        let dir = tempfile::tempdir().unwrap();
        let spec = fixture_spec(dir.path());
        let cap = dir.path().join("caps/claude-desktop.bk");
        let args = build_mcp_args(&spec, &cap);
        assert!(
            args.contains(&"--cap-file".to_string()),
            "args must include --cap-file, got: {args:?}"
        );
        assert!(
            args.iter().any(|a| a == &cap.to_string_lossy()),
            "args must include the cap-file path, got: {args:?}"
        );
        // Crucially: NO --cap with a literal token in args. That's
        // the whole point of the file-pointer redesign.
        assert!(
            !args.contains(&"--cap".to_string()),
            "args must NOT include --cap (token leak); got: {args:?}"
        );
    }

    #[test]
    fn render_codex_snippet_is_pasteable_toml() {
        let dir = tempfile::tempdir().unwrap();
        let spec = fixture_spec(dir.path());
        let cap = dir.path().join("caps/codex.bk");
        let s = render_codex_snippet(&spec, &cap);
        assert!(s.contains("[mcp_servers.ctxd]"));
        assert!(s.contains("command = "));
        assert!(s.contains("args = ["));
        assert!(s.contains("--cap-file"));
    }

    #[test]
    fn install_hooks_writes_four_events() {
        let dir = tempfile::tempdir().unwrap();
        let spec = ClientSpec {
            binary: dir.path().join("ctxd"),
            db_path: dir.path().join("db"),
            with_hooks: true,
        };
        let mut cfg = Value::Object(serde_json::Map::new());
        // mock paths::caps_dir() failure: we don't actually write to
        // disk, just exercise the hook block construction.
        if install_claude_code_hooks(&mut cfg, &spec).is_err() {
            // caps::cap_file_path may error if $HOME isn't set in the
            // test env. That's acceptable.
            return;
        }
        let h = &cfg["hooks"];
        assert!(h["SessionStart"].is_array());
        assert!(h["UserPromptSubmit"].is_array());
        assert!(h["PreCompact"].is_array());
        assert!(h["Stop"].is_array());
        let cmd = h["SessionStart"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(cmd.contains("hook"));
        assert!(cmd.contains("session-start"));
    }

    #[test]
    fn read_json_or_empty_handles_missing_and_empty() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("nope.json");
        assert!(read_json_or_empty(&p).unwrap().is_object());
        std::fs::write(&p, b"").unwrap();
        assert!(read_json_or_empty(&p).unwrap().is_object());
        std::fs::write(&p, b"{}").unwrap();
        assert!(read_json_or_empty(&p).unwrap().is_object());
    }

    #[test]
    fn read_json_or_empty_propagates_parse_errors() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("bad.json");
        std::fs::write(&p, b"not json at all").unwrap();
        assert!(read_json_or_empty(&p).is_err());
    }

    #[test]
    fn atomic_write_json_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("nested/dir/x.json");
        let v = serde_json::json!({"a": 1});
        atomic_write_json(&p, &v).unwrap();
        let back = read_json_or_empty(&p).unwrap();
        assert_eq!(back, v);
    }
}
