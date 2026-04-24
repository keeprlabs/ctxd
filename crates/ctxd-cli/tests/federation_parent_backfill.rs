//! Parent backfill: an inbound event references a parent the local
//! store doesn't have. The receiver issues `PeerFetchEvents` and
//! appends the parent + the child in topological order.

mod common;

use common::Daemon;
use ctxd_cli::federation::AutoAcceptPolicy;
use ctxd_cli::protocol::{ProtocolClient, Request, Response};
use ctxd_core::event::Event;
use ctxd_core::signing::EventSigner;
use ctxd_core::subject::Subject;

#[tokio::test]
async fn missing_parent_is_backfilled_via_peer_fetch_events() {
    let alice = Daemon::start_memory(AutoAcceptPolicy::Any).await;
    let bob = Daemon::start_memory(AutoAcceptPolicy::Any).await;

    // Two-way handshake.
    let _ = alice
        .dial_and_handshake(&bob, &["/work/**".to_string()])
        .await;
    let _ = bob
        .dial_and_handshake(&alice, &["/work/**".to_string()])
        .await;

    // Build two events on Alice's side directly: a parent and a child
    // that explicitly references the parent. The parent is appended
    // *only* on Alice (so Bob doesn't see it through normal channels).
    let signer = EventSigner::from_bytes(&alice.signing_key).expect("signer");
    let mut parent = Event::new(
        "ctxd://test".to_string(),
        Subject::new("/work/merge/p").expect("subj"),
        "demo".to_string(),
        serde_json::json!({"role": "parent"}),
    );
    parent.signature = Some(signer.sign(&parent).expect("sign"));
    let parent_stored = alice.store.append(parent.clone()).await.expect("append");
    let parent_id = parent_stored.id;

    // Child references the parent in `parents` field. Sign with parent
    // already present so signature canonicalizes correctly.
    let mut child = Event::new(
        "ctxd://test".to_string(),
        Subject::new("/work/merge/c").expect("subj"),
        "demo".to_string(),
        serde_json::json!({"role": "child"}),
    );
    child.parents = vec![parent_id];
    child.signature = Some(signer.sign(&child).expect("sign"));

    // Send the child to Bob via PeerReplicate. Bob doesn't have the
    // parent yet — but verify_inbound passes (signature + cap-scope ok)
    // and backfill kicks in.
    let event_json = serde_json::to_value(&child).expect("ser");
    let mut client = ProtocolClient::connect(&bob.addr.to_string())
        .await
        .expect("connect");
    let resp = client
        .request(&Request::PeerReplicate {
            origin_peer_id: alice.peer_id.clone(),
            event: event_json,
        })
        .await
        .expect("request");

    match resp {
        Response::Ok { .. } => {}
        other => panic!("expected Ok, got {other:?}"),
    }

    // Bob should now have the parent (backfilled) AND the child.
    let parent_subj = Subject::new("/work/merge/p").expect("subj");
    let bob_parents = bob.store.read(&parent_subj, false).await.expect("read");
    assert_eq!(
        bob_parents.len(),
        1,
        "bob should have the backfilled parent"
    );
    assert_eq!(bob_parents[0].id, parent_id);

    let child_subj = Subject::new("/work/merge/c").expect("subj");
    let bob_children = bob.store.read(&child_subj, false).await.expect("read");
    assert_eq!(bob_children.len(), 1, "bob should have the child");
    assert_eq!(bob_children[0].parents, vec![parent_id]);
}
