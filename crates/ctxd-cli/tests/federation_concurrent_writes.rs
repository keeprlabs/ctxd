//! Concurrent writes on the same subject from two peers must converge
//! deterministically under the KV LWW rule documented in ADR 006:
//! `(time, event_id)` lexicographic — higher wins.
//!
//! We deliberately fix the wall-clock times so the test is
//! deterministic: A's event is at t1, B's at t2 = t1 + 1ms. Whichever
//! event reaches a store later, kv_get must return the t2 value.

mod common;

use common::Daemon;
use ctxd_cli::federation::AutoAcceptPolicy;
use ctxd_cli::protocol::{ProtocolClient, Request, Response};
use ctxd_core::event::Event;
use ctxd_core::signing::EventSigner;
use ctxd_core::subject::Subject;

/// Submit a pre-built signed event over the wire with PeerReplicate.
async fn replicate(daemon: &Daemon, origin_peer_id: &str, event: &Event) {
    let mut client = ProtocolClient::connect(&daemon.addr.to_string())
        .await
        .expect("connect");
    let resp = client
        .request(&Request::PeerReplicate {
            origin_peer_id: origin_peer_id.to_string(),
            event: serde_json::to_value(event).expect("ser"),
        })
        .await
        .expect("request");
    match resp {
        Response::Ok { .. } => {}
        Response::Error { message } => panic!("replicate rejected: {message}"),
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test]
async fn concurrent_writes_converge_via_lww() {
    let alice = Daemon::start_memory(AutoAcceptPolicy::Any).await;
    let bob = Daemon::start_memory(AutoAcceptPolicy::Any).await;

    // Two-way handshake on /work/**.
    let _ = alice
        .dial_and_handshake(&bob, &["/work/**".to_string()])
        .await;
    let _ = bob
        .dial_and_handshake(&alice, &["/work/**".to_string()])
        .await;

    // Forge two concurrent events on the same subject:
    //   t1: alice writes value="A"
    //   t2: bob   writes value="B"   (1ms later)
    // Each is signed by its origin and replicated to the other.
    let t1 = chrono::Utc::now();
    let t2 = t1 + chrono::Duration::milliseconds(1);

    let alice_signer = EventSigner::from_bytes(&alice.signing_key).expect("a signer");
    let bob_signer = EventSigner::from_bytes(&bob.signing_key).expect("b signer");

    let mut a_event = Event::new(
        "ctxd://alice".to_string(),
        Subject::new("/work/lww/key").expect("subj"),
        "demo".to_string(),
        serde_json::json!({"value": "A"}),
    );
    a_event.time = t1;
    a_event.signature = Some(alice_signer.sign(&a_event).expect("sign A"));

    let mut b_event = Event::new(
        "ctxd://bob".to_string(),
        Subject::new("/work/lww/key").expect("subj"),
        "demo".to_string(),
        serde_json::json!({"value": "B"}),
    );
    b_event.time = t2;
    b_event.signature = Some(bob_signer.sign(&b_event).expect("sign B"));

    // Apply locally first.
    alice
        .store
        .append(a_event.clone())
        .await
        .expect("alice local A");
    bob.store
        .append(b_event.clone())
        .await
        .expect("bob local B");

    // Cross-replicate: A's event → B, B's event → A.
    replicate(&bob, &alice.peer_id, &a_event).await;
    replicate(&alice, &bob.peer_id, &b_event).await;

    // Both stores should now have the same KV value: B's (because t2 > t1).
    let alice_kv = alice
        .store
        .kv_get("/work/lww/key")
        .await
        .expect("kv a")
        .expect("present");
    let bob_kv = bob
        .store
        .kv_get("/work/lww/key")
        .await
        .expect("kv b")
        .expect("present");

    // Determinism: both sides must agree on the same value.
    assert_eq!(alice_kv, bob_kv, "stores must converge under LWW");

    // And the winning value must be the higher (time, id) — B at t2.
    assert_eq!(
        alice_kv,
        serde_json::json!({"value": "B"}),
        "B's event has higher time and must win"
    );
}

#[tokio::test]
async fn read_recursive_returns_both_branches_in_time_order() {
    let alice = Daemon::start_memory(AutoAcceptPolicy::Any).await;
    let bob = Daemon::start_memory(AutoAcceptPolicy::Any).await;

    let _ = alice
        .dial_and_handshake(&bob, &["/work/**".to_string()])
        .await;
    let _ = bob
        .dial_and_handshake(&alice, &["/work/**".to_string()])
        .await;

    let t1 = chrono::Utc::now();
    let t2 = t1 + chrono::Duration::milliseconds(2);

    let alice_signer = EventSigner::from_bytes(&alice.signing_key).expect("a signer");
    let bob_signer = EventSigner::from_bytes(&bob.signing_key).expect("b signer");

    let mut a_event = Event::new(
        "ctxd://alice".to_string(),
        Subject::new("/work/branch/k").expect("subj"),
        "demo".to_string(),
        serde_json::json!({"who": "alice"}),
    );
    a_event.time = t1;
    a_event.signature = Some(alice_signer.sign(&a_event).expect("sign A"));

    let mut b_event = Event::new(
        "ctxd://bob".to_string(),
        Subject::new("/work/branch/k").expect("subj"),
        "demo".to_string(),
        serde_json::json!({"who": "bob"}),
    );
    b_event.time = t2;
    b_event.signature = Some(bob_signer.sign(&b_event).expect("sign B"));

    alice.store.append(a_event.clone()).await.expect("a");
    bob.store.append(b_event.clone()).await.expect("b");
    replicate(&bob, &alice.peer_id, &a_event).await;
    replicate(&alice, &bob.peer_id, &b_event).await;

    let subj = Subject::new("/work/branch").expect("p");
    let mut alice_log = alice.store.read(&subj, true).await.expect("alice read");
    let mut bob_log = bob.store.read(&subj, true).await.expect("bob read");

    // Filter to our specific subject.
    alice_log.retain(|e| e.subject.as_str() == "/work/branch/k");
    bob_log.retain(|e| e.subject.as_str() == "/work/branch/k");

    assert_eq!(alice_log.len(), 2, "alice must see both branches");
    assert_eq!(bob_log.len(), 2, "bob must see both branches");

    // Sort each by (time, id) and ensure both sides agree.
    let key = |e: &Event| (e.time, e.id.to_string());
    alice_log.sort_by_key(key);
    bob_log.sort_by_key(key);

    let alice_ids: Vec<_> = alice_log.iter().map(|e| e.id).collect();
    let bob_ids: Vec<_> = bob_log.iter().map(|e| e.id).collect();
    assert_eq!(alice_ids, bob_ids, "ordering must be deterministic");
}
