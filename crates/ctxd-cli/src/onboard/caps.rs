//! Per-client capability minting + file-pointer persistence.
//!
//! Tokens are written to `<config_dir>/ctxd/caps/<client>.bk` with
//! file mode `0600` (Unix). Each MCP-spawning client launches `ctxd
//! serve --mcp-stdio --cap-file <path>` so the token never appears
//! in process arguments, the user's `claude_desktop_config.json`, or
//! `ps` output. This is the security fix from the phase-2A spec —
//! tokens in args are leak-prone (visible to any local process and
//! preserved in any backup of the config file).
//!
//! ## Per-client defaults
//!
//! With just `/me/` as the v0.4 taxonomy (per the open-questions
//! answer), every client gets the same broad scope by default and
//! `--strict-scopes` narrows it. Adapters get narrower scopes
//! always — they write into a single namespace they own.
//!
//! | Client          | Subjects        | Operations               |
//! |-----------------|-----------------|--------------------------|
//! | claude-desktop  | `/me/**`        | read, write, search, subjects |
//! | claude-code     | `/me/**`        | read, write, search, subjects |
//! | codex           | `/me/**`        | read, write, search, subjects |
//! | gmail-adapter   | `/me/inbox/**`  | write                    |
//! | github-adapter  | `/me/code/**`   | write                    |
//! | fs-adapter      | `/me/fs/**`     | write                    |
//!
//! With `--strict-scopes`, clients drop to `/me/**` read + search only
//! (no write, no subjects). The user grants write explicitly later
//! via `ctxd grant` if they want.

use anyhow::{Context, Result};
use ctxd_cap::{CapEngine, Operation};
use std::path::{Path, PathBuf};

use crate::onboard::paths;

/// Stable identifier for each onboarded client / adapter. Used as
/// the cap-file basename and the `clients_configured` /
/// `adapters_enabled` slugs in the protocol's `Outcome`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClientId {
    ClaudeDesktop,
    ClaudeCode,
    Codex,
    GmailAdapter,
    GithubAdapter,
    FsAdapter,
}

impl ClientId {
    /// Stable kebab-case slug used as the cap-file name and surfaced
    /// in protocol outcomes.
    pub fn slug(self) -> &'static str {
        match self {
            ClientId::ClaudeDesktop => "claude-desktop",
            ClientId::ClaudeCode => "claude-code",
            ClientId::Codex => "codex",
            ClientId::GmailAdapter => "gmail-adapter",
            ClientId::GithubAdapter => "github-adapter",
            ClientId::FsAdapter => "fs-adapter",
        }
    }

    /// Human-readable name for log lines and remediation strings.
    pub fn display(self) -> &'static str {
        match self {
            ClientId::ClaudeDesktop => "Claude Desktop",
            ClientId::ClaudeCode => "Claude Code",
            ClientId::Codex => "Codex",
            ClientId::GmailAdapter => "Gmail adapter",
            ClientId::GithubAdapter => "GitHub adapter",
            ClientId::FsAdapter => "fs adapter",
        }
    }

    /// Subject glob granted to this client / adapter. Strict mode
    /// keeps the scope (it's already `/me/**` for clients) but
    /// narrows the operations — see [`default_operations`].
    pub fn default_scope(self, _strict: bool) -> &'static str {
        match self {
            ClientId::ClaudeDesktop | ClientId::ClaudeCode | ClientId::Codex => "/me/**",
            ClientId::GmailAdapter => "/me/inbox/**",
            ClientId::GithubAdapter => "/me/code/**",
            ClientId::FsAdapter => "/me/fs/**",
        }
    }

    /// Operations granted to this client / adapter. Strict mode
    /// drops write + subjects from clients (read + search only) so a
    /// minted token can browse memory but not modify it; the user
    /// must explicitly run `ctxd grant` for write.
    pub fn default_operations(self, strict: bool) -> Vec<Operation> {
        match self {
            ClientId::ClaudeDesktop | ClientId::ClaudeCode | ClientId::Codex => {
                if strict {
                    vec![Operation::Read, Operation::Search]
                } else {
                    vec![
                        Operation::Read,
                        Operation::Write,
                        Operation::Search,
                        Operation::Subjects,
                    ]
                }
            }
            ClientId::GmailAdapter | ClientId::GithubAdapter | ClientId::FsAdapter => {
                // Adapters write only — they don't read or browse
                // (they ingest data; reading is done by clients).
                vec![Operation::Write]
            }
        }
    }
}

/// Where the cap-file for `client` lives on disk.
pub fn cap_file_path(client: ClientId) -> Result<PathBuf> {
    Ok(paths::caps_dir()?.join(format!("{}.bk", client.slug())))
}

/// Mint a capability token for `client` against `cap_engine` and
/// persist it to the canonical cap-file path. Returns the path.
///
/// Idempotent in spirit: a re-mint produces a new token (biscuits
/// have a fresh per-mint nonce) and overwrites the file. Callers
/// that want to detect "already minted" should check for the file
/// before calling.
pub fn mint_and_persist(cap_engine: &CapEngine, client: ClientId, strict: bool) -> Result<PathBuf> {
    let scope = client.default_scope(strict);
    let ops = client.default_operations(strict);
    let token = cap_engine
        .mint(scope, &ops, None, None, None)
        .with_context(|| format!("mint cap for {}", client.slug()))?;
    let b64 = CapEngine::token_to_base64(&token);
    let path = cap_file_path(client)?;
    write_cap_file(&path, b64.as_bytes())?;
    Ok(path)
}

/// Write `contents` to `path` with mode `0600` on Unix. Atomic via
/// write-temp-then-rename. Mode is set on the temp file BEFORE the
/// rename so the final path never has world-readable bits.
pub fn write_cap_file(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("cap file {path:?} has no parent dir"))?;
    std::fs::create_dir_all(parent).with_context(|| format!("create_dir_all {parent:?}"))?;
    let tmp = parent.join(format!(".cap-{}.tmp", std::process::id()));
    std::fs::write(&tmp, contents).with_context(|| format!("write {tmp:?}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&tmp)?.permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(&tmp, perms).with_context(|| format!("chmod 0600 {tmp:?}"))?;
    }
    std::fs::rename(&tmp, path).with_context(|| format!("rename {tmp:?} -> {path:?}"))?;
    Ok(())
}

/// Read a cap-file and return its base64-encoded token contents.
/// Trims a trailing newline if present (vi/nano often add one).
pub fn read_cap_file(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("read cap-file {path:?}"))?;
    let s = String::from_utf8(bytes).context("cap-file is not valid UTF-8")?;
    Ok(s.trim().to_string())
}

/// Verify a persisted cap-file: decodes successfully against
/// `cap_engine`, has a non-expired expiration (if any), and grants
/// at least one operation against the client's expected scope.
pub fn verify_persisted(cap_engine: &CapEngine, client: ClientId) -> Result<CapVerifyReport> {
    let path = cap_file_path(client)?;
    if !path.exists() {
        return Ok(CapVerifyReport {
            client,
            path,
            present: false,
            decodes: false,
            verifies_default_op: false,
            error: Some("cap file not present".into()),
        });
    }
    let b64 = match read_cap_file(&path) {
        Ok(s) => s,
        Err(e) => {
            return Ok(CapVerifyReport {
                client,
                path,
                present: true,
                decodes: false,
                verifies_default_op: false,
                error: Some(format!("read failed: {e:#}")),
            });
        }
    };
    let token = match CapEngine::token_from_base64(&b64) {
        Ok(t) => t,
        Err(e) => {
            return Ok(CapVerifyReport {
                client,
                path,
                present: true,
                decodes: false,
                verifies_default_op: false,
                error: Some(format!("base64 decode failed: {e}")),
            });
        }
    };
    // Pick a default op the client should have. For clients we
    // verify Read; for adapters we verify Write.
    let op = match client {
        ClientId::ClaudeDesktop | ClientId::ClaudeCode | ClientId::Codex => Operation::Read,
        ClientId::GmailAdapter | ClientId::GithubAdapter | ClientId::FsAdapter => Operation::Write,
    };
    // Try the default scope; if the user passed --strict-scopes
    // earlier, the same default scope still applies (we don't
    // narrow the path, only the ops).
    let scope = client.default_scope(false);
    let verify = cap_engine.verify(&token, scope, op, None);
    Ok(CapVerifyReport {
        client,
        path,
        present: true,
        decodes: true,
        verifies_default_op: verify.is_ok(),
        error: verify.err().map(|e| format!("verify failed: {e}")),
    })
}

/// Outcome of a cap-file verification.
#[derive(Debug, Clone)]
pub struct CapVerifyReport {
    pub client: ClientId,
    pub path: PathBuf,
    pub present: bool,
    pub decodes: bool,
    pub verifies_default_op: bool,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_kebab_case() {
        assert_eq!(ClientId::ClaudeDesktop.slug(), "claude-desktop");
        assert_eq!(ClientId::ClaudeCode.slug(), "claude-code");
        assert_eq!(ClientId::Codex.slug(), "codex");
        assert_eq!(ClientId::GmailAdapter.slug(), "gmail-adapter");
        assert_eq!(ClientId::GithubAdapter.slug(), "github-adapter");
        assert_eq!(ClientId::FsAdapter.slug(), "fs-adapter");
    }

    #[test]
    fn strict_clients_drop_write_and_subjects() {
        for c in [
            ClientId::ClaudeDesktop,
            ClientId::ClaudeCode,
            ClientId::Codex,
        ] {
            let strict = c.default_operations(true);
            let normal = c.default_operations(false);
            assert!(strict.contains(&Operation::Read));
            assert!(strict.contains(&Operation::Search));
            assert!(
                !strict.contains(&Operation::Write),
                "strict {c:?} must NOT have Write"
            );
            assert!(
                !strict.contains(&Operation::Subjects),
                "strict {c:?} must NOT have Subjects"
            );
            assert!(normal.contains(&Operation::Write));
            assert!(normal.contains(&Operation::Subjects));
        }
    }

    #[test]
    fn adapters_have_write_only() {
        for a in [
            ClientId::GmailAdapter,
            ClientId::GithubAdapter,
            ClientId::FsAdapter,
        ] {
            let ops = a.default_operations(false);
            assert_eq!(ops, vec![Operation::Write]);
            // Adapter scopes are narrower than /me/**.
            assert!(a.default_scope(false).starts_with("/me/"));
            assert!(a.default_scope(false).len() > "/me/**".len());
        }
    }

    #[test]
    fn write_cap_file_uses_mode_0600() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("cap.bk");
        write_cap_file(&p, b"secret").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "cap file must be 0600, got {mode:o}");
        }
        assert_eq!(std::fs::read(&p).unwrap(), b"secret");
    }

    #[test]
    fn write_cap_file_overwrites_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("cap.bk");
        write_cap_file(&p, b"v1").unwrap();
        write_cap_file(&p, b"v2").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"v2");
    }

    #[test]
    fn read_cap_file_trims_trailing_newline() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("cap.bk");
        write_cap_file(&p, b"abc\n").unwrap();
        assert_eq!(read_cap_file(&p).unwrap(), "abc");
    }

    #[test]
    fn mint_and_verify_round_trips_with_explicit_path() {
        // Don't call paths::caps_dir() (it depends on $HOME); test
        // write_cap_file + verify directly via lower-level APIs.
        let engine = CapEngine::new();
        let token = engine
            .mint(
                ClientId::ClaudeDesktop.default_scope(false),
                &ClientId::ClaudeDesktop.default_operations(false),
                None,
                None,
                None,
            )
            .unwrap();
        let b64 = CapEngine::token_to_base64(&token);
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("claude-desktop.bk");
        write_cap_file(&p, b64.as_bytes()).unwrap();
        let read = read_cap_file(&p).unwrap();
        let bytes = CapEngine::token_from_base64(&read).unwrap();
        // Verify it works for Read on /me/** (the default).
        engine
            .verify(&bytes, "/me/**", Operation::Read, None)
            .unwrap();
        // And for Write (in non-strict mode).
        engine
            .verify(&bytes, "/me/**", Operation::Write, None)
            .unwrap();
    }

    #[test]
    fn strict_token_rejects_write() {
        let engine = CapEngine::new();
        let token = engine
            .mint(
                ClientId::ClaudeCode.default_scope(true),
                &ClientId::ClaudeCode.default_operations(true),
                None,
                None,
                None,
            )
            .unwrap();
        // Read should pass.
        engine
            .verify(&token, "/me/**", Operation::Read, None)
            .expect("strict token should grant Read");
        // Write must fail.
        let err = engine
            .verify(&token, "/me/**", Operation::Write, None)
            .expect_err("strict token must NOT grant Write");
        let _ = err; // we only care that it errors
    }
}
