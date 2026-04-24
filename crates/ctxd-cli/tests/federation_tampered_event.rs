//! A peer replicates an event whose payload has been mutated after
//! signing. The receiver's signature check must reject it before
//! `Store::append` is reached.

mod common;

use common::Daemon;
use ctxd_cli::federation::AutoAcceptPolicy;
use ctxd_cli::protocol::{ProtocolClient, Request, Response};
use ctxd_core::event::Event;
use ctxd_core::signing::EventSigner;
use ctxd_core::subject::Subject;

#[tokio::test]
async fn replicated_event_with_bad_signature_is_rejected() {
    let alice = Daemon::start_memory(AutoAcceptPolicy::Any).await;
    let bob = Daemon::start_memory(AutoAcceptPolicy::Any).await;

    // Two-way handshake.
    let _ = alice
        .dial_and_handshake(&bob, &["/work/**".to_string()])
        .await;
    let _ = bob
        .dial_and_handshake(&alice, &["/work/**".to_string()])
        .await;

    // Build an event signed by Alice.
    let signer = EventSigner::from_bytes(&alice.signing_key).expect("signer");
    let mut event = Event::new(
        "ctxd://test".to_string(),
        Subject::new("/work/note/tampered").expect("subj"),
        "demo".to_string(),
        serde_json::json!({"msg": "original"}),
    );
    event.signature = Some(signer.sign(&event).expect("sign"));

    // Tamper with the payload AFTER signing — signature no longer matches.
    event.data = serde_json::json!({"msg": "tampered"});

    let event_json = serde_json::to_value(&event).expect("ser");

    // Hand-craft a PeerReplicate to Bob with the tampered event.
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
                message.contains("signature") || message.contains("invalid"),
                "expected signature-related error, got: {message}"
            );
        }
        other => panic!("expected Error response, got {other:?}"),
    }

    // Bob's store must NOT contain the tampered event.
    let bob_events = bob
        .store
        .read(&Subject::new("/work/note/tampered").unwrap(), false)
        .await
        .expect("read");
    assert!(
        bob_events.is_empty(),
        "bob should have no event after tamper"
    );
}
