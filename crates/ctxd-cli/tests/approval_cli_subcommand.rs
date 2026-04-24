//! Integration test: `ctxd approve <id> --decision allow|deny` writes
//! into the daemon's database and the running daemon's
//! [`SqliteCaveatState`] sees the result.
//!
//! We invoke the binary via `cargo run --bin ctxd` in a tempdir so we
//! exercise the actual clap parsing and the same code path operators
//! will hit. Then we re-open the same database from the test process
//! and assert the row decided.

use std::process::Command;

use ctxd_cap::state::{ApprovalDecision, CaveatState};
use ctxd_store_sqlite::caveat_state::SqliteCaveatState;
use ctxd_store_sqlite::EventStore;
use tempfile::TempDir;

fn cargo_run_args() -> Command {
    // Use `cargo run --bin ctxd --quiet --` so the test invokes the
    // binary that ships with this workspace, not whatever is on PATH.
    let mut cmd = Command::new(env!("CARGO"));
    cmd.args(["run", "--quiet", "--bin", "ctxd", "--"]);
    cmd
}

#[tokio::test]
async fn approve_cli_records_allow() {
    let tmp = TempDir::new().expect("tempdir");
    let db_path = tmp.path().join("ctxd.db");

    // Pre-seed an approval row directly via the store. We do this
    // through `EventStore` + `SqliteCaveatState` rather than running
    // a daemon; the daemon would otherwise need stdio fixtures.
    {
        let store = EventStore::open(&db_path).await.expect("open");
        let st = SqliteCaveatState::new(store);
        st.approval_request("appr-cli-1", "tok", "write", "/work/x")
            .await
            .expect("seed");
    }

    // Invoke `ctxd approve --id appr-cli-1 --decision allow`.
    let output = cargo_run_args()
        .arg("--db")
        .arg(&db_path)
        .arg("approve")
        .arg("--id")
        .arg("appr-cli-1")
        .arg("--decision")
        .arg("allow")
        .output()
        .expect("spawn ctxd binary");
    assert!(
        output.status.success(),
        "ctxd approve exited non-zero: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Re-open the database in-test and confirm the row was decided.
    let store = EventStore::open(&db_path).await.expect("re-open");
    let st = SqliteCaveatState::new(store);
    let status = st
        .approval_status("appr-cli-1")
        .await
        .expect("approval row");
    assert_eq!(status, ApprovalDecision::Allow);
}

#[tokio::test]
async fn approve_cli_rejects_invalid_decision() {
    let tmp = TempDir::new().expect("tempdir");
    let db_path = tmp.path().join("ctxd.db");
    {
        let store = EventStore::open(&db_path).await.expect("open");
        let st = SqliteCaveatState::new(store);
        st.approval_request("appr-cli-2", "tok", "write", "/work/x")
            .await
            .expect("seed");
    }

    let output = cargo_run_args()
        .arg("--db")
        .arg(&db_path)
        .arg("approve")
        .arg("--id")
        .arg("appr-cli-2")
        .arg("--decision")
        .arg("maybe")
        .output()
        .expect("spawn");
    assert!(
        !output.status.success(),
        "ctxd approve --decision maybe must fail; got success with stdout={}",
        String::from_utf8_lossy(&output.stdout)
    );
}
