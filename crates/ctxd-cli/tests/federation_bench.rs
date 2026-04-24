//! Federation benchmarks. Run with `cargo test --release -p ctxd-cli
//! --test federation_bench -- --ignored --nocapture` to get numbers.
//!
//! Two metrics:
//! - One-way replication throughput (events/sec) over localhost TCP.
//! - Third-party block verification latency (us/verification).

mod common;

use common::Daemon;
use ctxd_cap::{BiscuitKeyPair, CapEngine, Caveat, Operation};
use ctxd_cli::federation::AutoAcceptPolicy;
use std::time::Instant;

#[tokio::test]
#[ignore]
async fn bench_replication_throughput() {
    let alice = Daemon::start_memory(AutoAcceptPolicy::Any).await;
    let bob = Daemon::start_memory(AutoAcceptPolicy::Any).await;

    let _ = alice
        .dial_and_handshake(&bob, &["/work/**".to_string()])
        .await;
    let _ = bob
        .dial_and_handshake(&alice, &["/work/**".to_string()])
        .await;

    let n = 1000;
    let mut last_id = uuid::Uuid::nil();

    let start = Instant::now();
    for i in 0..n {
        let stored = alice
            .pub_event(
                "/work/bench/x",
                "demo",
                serde_json::json!({"i": i, "payload": "hello world"}),
            )
            .await;
        last_id = stored.id;
    }

    // Wait for the last event to appear on Bob — that defines
    // end-to-end latency for the burst.
    let saw_last = bob
        .wait_for_event(last_id, std::time::Duration::from_secs(60))
        .await;
    let elapsed = start.elapsed();

    assert!(saw_last, "bob did not receive the last event in time");

    let per_sec = (n as f64) / elapsed.as_secs_f64();
    println!(
        "[bench] replicated {n} events one-way over localhost TCP in {:?} ({:.0} events/sec)",
        elapsed, per_sec
    );
}

#[test]
#[ignore]
fn bench_third_party_verify_latency() {
    let engine = CapEngine::new();
    let kp_b = BiscuitKeyPair::new();
    let kp_c = BiscuitKeyPair::new();

    // Build a 3-authority chain: root → B → C.
    let token = engine
        .mint(
            "/**",
            &[Operation::Read, Operation::Write],
            None,
            None,
            None,
        )
        .expect("mint");
    let token_b = engine
        .attenuate_with_block(
            &token,
            &kp_b.private(),
            &[Caveat::SubjectPrefix("/work/**".to_string())],
        )
        .expect("B");
    let token_c = engine
        .attenuate_with_block(
            &token_b,
            &kp_c.private(),
            &[Caveat::OperationsAtMost(vec![Operation::Read])],
        )
        .expect("C");

    let trust = vec![kp_b.public(), kp_c.public()];

    // Warm-up.
    for _ in 0..100 {
        engine
            .verify_multi(&token_c, &trust, "/work/team1/doc", Operation::Read)
            .expect("warm");
    }

    let n = 1000;
    let start = Instant::now();
    for _ in 0..n {
        engine
            .verify_multi(&token_c, &trust, "/work/team1/doc", Operation::Read)
            .expect("verify");
    }
    let elapsed = start.elapsed();
    let avg_us = elapsed.as_micros() as f64 / n as f64;
    println!(
        "[bench] verify_multi 3-authority chain: {} verifies in {:?}, avg {:.1} us/verify",
        n, elapsed, avg_us
    );
}
