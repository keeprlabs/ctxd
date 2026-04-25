//! Loop guard: in a three-node ring (A → B → C → A), an event published
//! on A should make exactly one full lap and stop. No infinite replay.

mod common;

use common::Daemon;
use ctxd_cli::federation::AutoAcceptPolicy;

#[tokio::test]
async fn three_node_ring_event_makes_one_lap() {
    let a = Daemon::start_memory(AutoAcceptPolicy::Any).await;
    let b = Daemon::start_memory(AutoAcceptPolicy::Any).await;
    let c = Daemon::start_memory(AutoAcceptPolicy::Any).await;

    // Wire a closed ring: A↔B, B↔C, C↔A.
    let _ = a.dial_and_handshake(&b, &["/work/**".to_string()]).await;
    let _ = b.dial_and_handshake(&a, &["/work/**".to_string()]).await;
    let _ = b.dial_and_handshake(&c, &["/work/**".to_string()]).await;
    let _ = c.dial_and_handshake(&b, &["/work/**".to_string()]).await;
    let _ = c.dial_and_handshake(&a, &["/work/**".to_string()]).await;
    let _ = a.dial_and_handshake(&c, &["/work/**".to_string()]).await;

    // A publishes one event.
    let stored = a
        .pub_event("/work/ring/1", "demo", serde_json::json!({"hop": 0}))
        .await;

    // B and C should each receive it once (and exactly once). Wait
    // long enough for any infinite loop to manifest.
    assert!(
        b.wait_for_event(stored.id, std::time::Duration::from_secs(3))
            .await,
        "B never received the event"
    );
    assert!(
        c.wait_for_event(stored.id, std::time::Duration::from_secs(3))
            .await,
        "C never received the event"
    );

    // Allow some quiescence, then assert each store has exactly 1 row.
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let subj = ctxd_core::subject::Subject::new("/work/ring/1").expect("subj");
    let a_events = a.store.read(&subj, false).await.expect("read a");
    let b_events = b.store.read(&subj, false).await.expect("read b");
    let c_events = c.store.read(&subj, false).await.expect("read c");

    assert_eq!(a_events.len(), 1, "A has dup events: {}", a_events.len());
    assert_eq!(b_events.len(), 1, "B has dup events: {}", b_events.len());
    assert_eq!(c_events.len(), 1, "C has dup events: {}", c_events.len());
}
