//! A peer replicates an event whose subject is outside its granted
//! glob. The receiver must reject it before `Store::append`.

mod common;

use common::Daemon;
use ctxd_cli::federation::AutoAcceptPolicy;
use ctxd_cli::protocol::{ProtocolClient, Request, Response};
use ctxd_core::event::Event;
use ctxd_core::signing::EventSigner;
use ctxd_core::subject::Subject;

#[tokio::test]
async fn peer_writing_outside_granted_scope_is_rejected() {
    let alice = Daemon::start_memory(AutoAcceptPolicy::Any).await;
    let bob = Daemon::start_memory(AutoAcceptPolicy::Any).await;

    // Bob grants Alice ONLY /work/**. Anything outside should be denied.
    let _ = alice
        .dial_and_handshake(&bob, &["/work/**".to_string()])
        .await;
    let _ = bob
        .dial_and_handshake(&alice, &["/work/**".to_string()])
        .await;

    // Sign an event under /home/secret — outside the granted scope.
    let signer = EventSigner::from_bytes(&alice.signing_key).expect("signer");
    let mut event = Event::new(
        "ctxd://test".to_string(),
        Subject::new("/home/secret").expect("subj"),
        "demo".to_string(),
        serde_json::json!({"steal": "ok"}),
    );
    event.signature = Some(signer.sign(&event).expect("sign"));
    let event_json = serde_json::to_value(&event).expect("ser");

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
        Response::Error { message } => {
            assert!(
                message.contains("scope") || message.contains("/home/secret"),
                "expected scope violation error, got: {message}"
            );
        }
        other => panic!("expected Error response, got {other:?}"),
    }

    let bob_events = bob
        .store
        .read(&Subject::new("/home/secret").unwrap(), false)
        .await
        .expect("read");
    assert!(
        bob_events.is_empty(),
        "bob should not have stored the event"
    );
}
