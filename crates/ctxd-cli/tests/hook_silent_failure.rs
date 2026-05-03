//! Hooks must never surface errors to Claude Code.
//!
//! `ctxd hook <slug>` is wired into `~/.claude/settings.json` by
//! `ctxd onboard --with-hooks`. Once written, a hook entry can outlive
//! its DB target — e.g. a sandboxed onboard run with
//! `CTXD_DATA_DIR=$(mktemp -d)` writes a hook pointing at the tempdir,
//! the tempdir is later wiped, and the hook keeps firing on every
//! Claude Code session end. If the hook propagates SQLITE_CANTOPEN
//! (code 14) as a non-zero exit, Claude Code prints "Stop hook error"
//! into the user's chat on every Stop event — a permanent visible
//! failure mode they can only break out of by re-running onboard.
//!
//! Contract: the hook subcommand is best-effort. Any failure (DB path
//! resolves to a wiped tempdir, parent dir missing, exclusive lock,
//! permission denied, malformed slug, malformed payload) MUST exit 0
//! with a stderr warn. The happy path appends one event under
//! `/me/sessions/<slug>` and exits 0.

use std::io::Write;
use std::process::{Command, Stdio};

fn ctxd_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ctxd")
}

fn run_hook_with_stdin(args: &[&str], stdin_payload: &[u8]) -> std::process::Output {
    let mut child = Command::new(ctxd_bin())
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn ctxd");
    if let Some(mut sin) = child.stdin.take() {
        sin.write_all(stdin_payload).expect("write stdin");
    }
    child.wait_with_output().expect("wait")
}

#[test]
fn hook_exits_zero_when_db_path_does_not_exist() {
    // Reproduces the real-world failure: settings.json points at a
    // tempdir that was wiped after a sandboxed onboard run.
    let nonexistent = "/var/folders/__ctxd_does_not_exist__/ctxd.db";
    let out = run_hook_with_stdin(
        &[
            "--db",
            nonexistent,
            "hook",
            "--cap-file",
            "/tmp/whatever-missing.bk",
            "stop",
        ],
        br#"{"session_id":"test","hook_event_name":"Stop"}"#,
    );
    assert!(
        out.status.success(),
        "exit={:?} stdout={} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("ctxd hook:") && stderr.contains("best-effort"),
        "expected best-effort warn on stderr, got: {stderr}"
    );
}

#[test]
fn hook_exits_zero_when_db_parent_dir_does_not_exist() {
    // Even more aggressive: parent dir doesn't exist, so SQLite can't
    // create the journal. Still must not propagate.
    let dir = tempfile::tempdir().expect("tempdir");
    let phantom = dir.path().join("nope/never/created/ctxd.db");
    let out = run_hook_with_stdin(
        &[
            "--db",
            phantom.to_str().unwrap(),
            "hook",
            "--cap-file",
            "/tmp/whatever.bk",
            "stop",
        ],
        b"{}",
    );
    assert!(
        out.status.success(),
        "exit={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
}

#[test]
fn hook_appends_event_on_happy_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("ctxd.db");
    let out = run_hook_with_stdin(
        &[
            "--db",
            db.to_str().unwrap(),
            "hook",
            "--cap-file",
            "/tmp/whatever.bk",
            "user-prompt-submit",
        ],
        br#"{"session_id":"abc","prompt":"hi"}"#,
    );
    assert!(out.status.success(), "hook should succeed: {out:?}");

    // Read it back via the same binary so we exercise the full
    // round-trip rather than a library shortcut.
    let read = Command::new(ctxd_bin())
        .args([
            "--db",
            db.to_str().unwrap(),
            "read",
            "--subject",
            "/me/sessions/user-prompt-submit",
        ])
        .output()
        .expect("read");
    assert!(read.status.success(), "read failed: {read:?}");
    let body = String::from_utf8_lossy(&read.stdout);
    assert!(
        body.contains("hook.user-prompt-submit"),
        "expected event type in output: {body}"
    );
    assert!(
        body.contains("\"prompt\": \"hi\""),
        "expected payload in output: {body}"
    );
}

#[test]
fn hook_handles_invalid_slug_silently() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("ctxd.db");
    let out = run_hook_with_stdin(
        &[
            "--db",
            db.to_str().unwrap(),
            "hook",
            "--cap-file",
            "/tmp/whatever.bk",
            // Slug with characters that break Subject parsing — e.g.
            // a leading slash would generate a malformed
            // /me/sessions//foo path.
            "/bad-slug",
        ],
        b"{}",
    );
    assert!(
        out.status.success(),
        "invalid slug must not propagate: {out:?}"
    );
}

#[test]
fn hook_handles_non_json_payload_silently() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("ctxd.db");
    let out = run_hook_with_stdin(
        &[
            "--db",
            db.to_str().unwrap(),
            "hook",
            "--cap-file",
            "/tmp/whatever.bk",
            "stop",
        ],
        b"this is not json at all",
    );
    assert!(
        out.status.success(),
        "non-json payload must not propagate: {out:?}"
    );
}
