//! Replication: an event published on A reaches B with byte-identical
//! id, predecessor hash, signature, and parents.

mod common;

use common::Daemon;
use ctxd_cli::federation::AutoAcceptPolicy;

#[tokio::test]
async fn write_on_a_appears_on_b() {
    let alice = Daemon::start_memory(AutoAcceptPolicy::Any).await;
    let bob = Daemon::start_memory(AutoAcceptPolicy::Any).await;

    // Two-way handshake so each side knows the other.
    let _ = alice
        .dial_and_handshake(&bob, &["/work/**".to_string()])
        .await;
    let _ = bob
        .dial_and_handshake(&alice, &["/work/**".to_string()])
        .await;

    // Alice publishes an event under /work/note/1.
    let stored = alice
        .pub_event(
            "/work/note/1",
            "demo",
            serde_json::json!({"msg": "hi from A"}),
        )
        .await;

    // Bob should see it within 2s.
    assert!(
        bob.wait_for_event(stored.id, std::time::Duration::from_secs(3))
            .await,
        "bob never saw alice's event"
    );

    // Re-read from Bob's store and compare full event.
    let root = ctxd_core::subject::Subject::new("/work/note/1").expect("subject");
    let bob_events = bob.store.read(&root, false).await.expect("read");
    assert_eq!(bob_events.len(), 1, "bob should have exactly one event");
    let bob_event = &bob_events[0];

    assert_eq!(bob_event.id, stored.id, "id must match");
    assert_eq!(
        bob_event.predecessorhash, stored.predecessorhash,
        "predecessor hash must match"
    );
    assert_eq!(
        bob_event.signature, stored.signature,
        "signature must match"
    );
    assert_eq!(bob_event.parents, stored.parents, "parents must match");
    assert_eq!(
        bob_event.subject.as_str(),
        stored.subject.as_str(),
        "subject must match"
    );
}
