//! End-to-end tests for `ctxd onboard` mode flags (phase 3C).
//!
//! Each test asserts the contract one flag promises:
//!
//! * `--strict-scopes` → minted client caps grant Read but reject
//!   Write on `/me/**`.
//! * `--dry-run` → no mutations on disk anywhere.
//! * `--skill-mode --headless` → stdout is well-formed JSON Lines
//!   only; no prompt-shaped lines, no human formatting.
//! * `--only=<steps>` → exactly those step slugs appear in the JSON
//!   stream (snapshot/doctor were already in onboard_e2e; this test
//!   covers a non-trivial multi-step subset).
//! * Idempotent re-run → onboarding twice doesn't pile up snapshots
//!   or duplicate seeds.
//!
//! Tests use `--skip-service` so they don't touch launchd/systemd
//! on the test host. Each test sets `CTXD_CONFIG_DIR` and
//! `CTXD_DATA_DIR` to per-test tempdirs so parallel test execution
//! doesn't race on the global `<config_dir>/ctxd/caps/` path —
//! before this isolation, two parallel tests would each mint caps
//! signed by their own root_key and overwrite each other's files,
//! making whichever test ran later see a verify failure.

use ctxd_cap::{CapEngine, Operation};
use ctxd_cli::onboard::caps;
use serde_json::Value;
use std::process::Stdio;

fn ctxd_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ctxd")
}

fn parse_jsonl(stdout: &str) -> Vec<Value> {
    stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .collect()
}

/// Run `ctxd onboard` with the given flags inside a fully isolated
/// per-test config + data dir. Returns (output, db_path,
/// config_dir_keepalive). The keepalive guard owns the tempdir; the
/// caller drops it after the test.
async fn run_onboard_with_db(
    args: &[&str],
) -> (std::process::Output, std::path::PathBuf, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("ctxd.db");
    // Per-test cap files / skills.toml / snapshots — without this
    // override, parallel tests race on the global Application Support
    // path and clobber each other's caps.
    let config = dir.path().join("config");
    let data = dir.path().join("data");
    std::fs::create_dir_all(&config).expect("create config dir");
    std::fs::create_dir_all(&data).expect("create data dir");

    let mut full_args = vec!["--db", db.to_str().unwrap(), "onboard", "--skip-service"];
    full_args.extend_from_slice(args);
    let out = tokio::process::Command::new(ctxd_bin())
        .env("CTXD_CONFIG_DIR", &config)
        .env("CTXD_DATA_DIR", &data)
        .args(&full_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("spawn ctxd onboard");
    (out, db, dir)
}

#[tokio::test]
async fn strict_scopes_caps_grant_read_but_reject_write() {
    let (out, db, dir) = run_onboard_with_db(&[
        "--skip-adapters",
        "--skill-mode",
        "--strict-scopes",
        "--bind",
        "127.0.0.1:0",
        "--wire-bind",
        "127.0.0.1:0",
    ])
    .await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let messages = parse_jsonl(&stdout);
    assert!(
        messages.iter().any(|m| m["kind"] == "done"),
        "no done message; stdout=\n{stdout}"
    );
    // Note: with --skip-service the daemon-running and
    // service-installed doctor checks fail, so the binary exits 1.
    // That's correct user-facing behavior — the test asserts the
    // CAP shape, not the overall onboarded bit.

    // Load the daemon's root key from the temp DB and verify each
    // client's persisted cap directly. Strict mode means clients
    // get Read+Search only — no Write.
    let store = ctxd_store::EventStore::open(&db).await.expect("open store");
    let root_key = store
        .get_metadata("root_key")
        .await
        .expect("read root_key")
        .expect("root_key persisted");
    let cap_engine = CapEngine::from_private_key(&root_key).expect("root_key valid");

    // Read caps from the per-test config dir, not the global one —
    // CTXD_CONFIG_DIR was passed to the subprocess so its mint
    // wrote here.
    let caps_dir = dir.path().join("config").join("caps");

    for (client, slug) in [
        (caps::ClientId::ClaudeDesktop, "claude-desktop"),
        (caps::ClientId::ClaudeCode, "claude-code"),
        (caps::ClientId::Codex, "codex"),
    ] {
        let path = caps_dir.join(format!("{slug}.bk"));
        assert!(path.exists(), "cap file missing for {client:?} at {path:?}");
        let b64 = caps::read_cap_file(&path).expect("read cap");
        let token = CapEngine::token_from_base64(&b64).expect("decode cap");
        // Read on /me/** must succeed.
        cap_engine
            .verify(&token, "/me/**", Operation::Read, None)
            .unwrap_or_else(|e| panic!("strict {client:?} should grant Read: {e}"));
        // Write on /me/** must fail.
        let write_result = cap_engine.verify(&token, "/me/**", Operation::Write, None);
        assert!(
            write_result.is_err(),
            "strict {client:?} cap MUST NOT grant Write"
        );
    }
}

#[tokio::test]
async fn dry_run_makes_no_mutations() {
    // Capture the Application Support state before/after to verify
    // dry_run leaves it untouched. We can't fully isolate because
    // the cap_file_path is global on macOS, but we can assert that
    // the daemon's tempdir DB doesn't grow events and that the
    // step messages are all `skipped`.
    let (out, db, _dir) = run_onboard_with_db(&[
        "--skip-adapters",
        "--skill-mode",
        "--dry-run",
        "--bind",
        "127.0.0.1:0",
        "--wire-bind",
        "127.0.0.1:0",
    ])
    .await;
    // dry-run still triggers the doctor at the end, which fails
    // because no daemon is running. The exit code is 1, but the
    // contract under test is that NOTHING was mutated — the
    // assertions below cover that.
    let _ = out.status;
    // The DB might be opened by the binary (main.rs eagerly opens),
    // so the file may exist — but the seed-subjects step was
    // skipped, so the log must be empty.
    if db.exists() {
        let store = ctxd_store::EventStore::open(&db).await.expect("open");
        let count = store.event_count().await.expect("count");
        assert_eq!(count, 0, "dry-run must leave the log empty (got {count})");
    }
    // Every step's terminal status should be skipped or started — never ok.
    let stdout = String::from_utf8_lossy(&out.stdout);
    let messages = parse_jsonl(&stdout);
    let ok_steps: Vec<&str> = messages
        .iter()
        .filter(|m| m["kind"] == "step" && m["status"] == "ok")
        .filter_map(|m| m["step"].as_str())
        .collect();
    // Only `doctor` is allowed to be `ok` in dry-run because it's a
    // read-only step that runs the diagnostic suite.
    for step in &ok_steps {
        assert_eq!(
            *step, "doctor",
            "dry-run produced ok status for non-doctor step `{step}`"
        );
    }
}

#[tokio::test]
async fn skill_mode_stdout_is_pure_jsonl_no_human_formatting() {
    let (out, _db, _dir) = run_onboard_with_db(&[
        "--skip-adapters",
        "--skill-mode",
        "--bind",
        "127.0.0.1:0",
        "--wire-bind",
        "127.0.0.1:0",
    ])
    .await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Every non-empty line must parse as JSON. The skill's parser
    // assumes this — any human-format leakage breaks the contract.
    for (idx, line) in stdout.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let parsed: Result<Value, _> = serde_json::from_str(line);
        assert!(
            parsed.is_ok(),
            "stdout line {idx} is not valid JSON: {line:?}"
        );
        let v = parsed.unwrap();
        // Every message must have kind + protocol per the contract.
        assert!(v.get("kind").is_some(), "line {idx} missing `kind`: {line}");
        assert_eq!(v["protocol"], 1, "line {idx} has wrong protocol: {line}");
    }
}

#[tokio::test]
async fn only_filter_runs_exactly_the_listed_steps() {
    // A non-trivial subset: snapshot + mint-capabilities + doctor.
    // Excludes everything else.
    let (out, _db, _dir) = run_onboard_with_db(&[
        "--skip-adapters",
        "--skill-mode",
        "--only",
        "snapshot,mint-capabilities,doctor",
        "--bind",
        "127.0.0.1:0",
        "--wire-bind",
        "127.0.0.1:0",
    ])
    .await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let messages = parse_jsonl(&stdout);
    let step_slugs: std::collections::HashSet<&str> = messages
        .iter()
        .filter(|m| m["kind"] == "step")
        .filter_map(|m| m["step"].as_str())
        .collect();

    let allowed: std::collections::HashSet<&str> = ["snapshot", "mint-capabilities", "doctor"]
        .into_iter()
        .collect();
    for s in &step_slugs {
        assert!(
            allowed.contains(s),
            "--only filter let `{s}` through; expected only {allowed:?}"
        );
    }
    for required in &allowed {
        assert!(
            step_slugs.contains(required),
            "--only excluded `{required}` from the stream"
        );
    }
}

#[tokio::test]
async fn idempotent_rerun_does_not_duplicate_seeds() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("ctxd.db");
    let config = dir.path().join("config");
    let data = dir.path().join("data");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    let common_args: Vec<&str> = vec![
        "--db",
        db.to_str().unwrap(),
        "onboard",
        "--skip-service",
        "--skip-adapters",
        "--skill-mode",
        "--bind",
        "127.0.0.1:0",
        "--wire-bind",
        "127.0.0.1:0",
    ];

    // First run.
    let out1 = tokio::process::Command::new(ctxd_bin())
        .env("CTXD_CONFIG_DIR", &config)
        .env("CTXD_DATA_DIR", &data)
        .args(&common_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("spawn 1");
    let _ = out1.status;

    // Count events in the log.
    let store = ctxd_store::EventStore::open(&db).await.expect("open");
    let count_after_first = store.event_count().await.expect("count");
    assert!(
        count_after_first >= 3,
        "first onboard should write at least 3 seed events, got {count_after_first}"
    );
    drop(store);

    // Second run.
    let out2 = tokio::process::Command::new(ctxd_bin())
        .env("CTXD_CONFIG_DIR", &config)
        .env("CTXD_DATA_DIR", &data)
        .args(&common_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("spawn 2");
    let _ = out2.status;

    let store = ctxd_store::EventStore::open(&db).await.expect("reopen");
    let count_after_second = store.event_count().await.expect("count");
    // The seed step must be idempotent — re-onboarding should not
    // pile up seed events. (Some events may be added by the
    // pipeline's other steps, e.g. cap-mint metadata; the important
    // assertion is that seeds didn't multiply by 2.)
    assert!(
        count_after_second < count_after_first * 2,
        "re-onboard appeared to double seeds: {count_after_first} → {count_after_second}"
    );
}
