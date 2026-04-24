//! Cursor resume: kill B mid-replication, restart, and assert
//! catch_up_peer replays without duplicates or gaps.

mod common;

use common::Daemon;
use ctxd_cli::federation::AutoAcceptPolicy;
use ctxd_core::subject::Subject;

#[tokio::test]
async fn cursor_resume_replays_backlog_without_duplicates() {
    let alice = Daemon::start_memory(AutoAcceptPolicy::Any).await;
    let bob = Daemon::start_memory(AutoAcceptPolicy::Any).await;

    // Two-way handshake.
    let _ = alice
        .dial_and_handshake(&bob, &["/work/**".to_string()])
        .await;
    let _ = bob
        .dial_and_handshake(&alice, &["/work/**".to_string()])
        .await;

    // Alice publishes 3 events. Wait for replication.
    for i in 0..3 {
        let stored = alice
            .pub_event("/work/note/x", "demo", serde_json::json!({"step": i}))
            .await;
        assert!(
            bob.wait_for_event(stored.id, std::time::Duration::from_secs(2))
                .await,
            "step {i} did not replicate"
        );
    }

    // Snapshot Bob's count.
    let subj = Subject::new("/work/note/x").expect("subj");
    let bob_count_before = bob.store.read(&subj, false).await.expect("read").len();
    assert_eq!(bob_count_before, 3, "bob should have all 3 events");

    // Now write 2 more events on Alice WITHOUT bob being able to receive
    // — easiest way: just call store.append directly so the broadcast
    // never happens. This simulates "Bob was offline."
    let mut to_replicate_ids = Vec::new();
    for i in 3..5 {
        use ctxd_core::event::Event;
        let event = Event::new(
            "ctxd://test".to_string(),
            Subject::new("/work/note/x").expect("subj"),
            "demo".to_string(),
            serde_json::json!({"step": i}),
        );
        let stored = alice.store.append(event).await.expect("append");
        to_replicate_ids.push(stored.id);
    }

    // Bob should still have only 3.
    let bob_mid = bob.store.read(&subj, false).await.expect("read").len();
    assert_eq!(bob_mid, 3, "bob should not have the offline events yet");

    // Force a catch-up. Should sync the 2 backlog events.
    let peers = alice.fed.list_peers().await;
    assert_eq!(peers.len(), 1);
    let result = alice.fed.catch_up_peer(&peers[0]).await;
    assert!(result.is_ok(), "catch-up errored: {:?}", result.err());

    // Wait briefly for delivery to settle.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Bob should now have all 5 — no duplicates, no gaps.
    let bob_after = bob.store.read(&subj, false).await.expect("read");
    assert_eq!(bob_after.len(), 5, "bob should have all 5 after catch-up");

    // Both backlog events should be present.
    for id in &to_replicate_ids {
        assert!(
            bob_after.iter().any(|e| e.id == *id),
            "missing backfilled event {id}"
        );
    }

    // Run catch_up again; should be a no-op (cursor already at the head).
    let result = alice.fed.catch_up_peer(&peers[0]).await;
    assert!(result.is_ok());
    let bob_final = bob.store.read(&subj, false).await.expect("read");
    assert_eq!(
        bob_final.len(),
        5,
        "second catch-up must not produce duplicates"
    );
}
