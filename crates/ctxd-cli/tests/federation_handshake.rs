//! Two in-process daemons complete a `peer add` handshake and both
//! sides persist the peer record (pubkey + granted subjects).

mod common;

use common::Daemon;
use ctxd_cli::federation::AutoAcceptPolicy;

#[tokio::test]
async fn two_daemons_handshake_and_persist_peer() {
    let alice = Daemon::start_memory(AutoAcceptPolicy::Any).await;
    let bob = Daemon::start_memory(AutoAcceptPolicy::Any).await;

    // Alice dials Bob, granting /work/**.
    let enrolled = alice
        .dial_and_handshake(&bob, &["/work/**".to_string()])
        .await;

    // Welcome must carry Bob's pubkey.
    let bob_pk_hex = hex::encode(&enrolled.remote_pubkey);
    assert_eq!(bob_pk_hex, bob.peer_id, "welcome must surface Bob's pubkey");

    // Both sides must have persisted the peer row.
    let alice_peers = alice.store.peer_list_impl().await.expect("list a");
    let bob_peers = bob.store.peer_list_impl().await.expect("list b");

    assert_eq!(alice_peers.len(), 1, "alice should have one peer");
    assert_eq!(alice_peers[0].peer_id, bob.peer_id);
    assert_eq!(
        alice_peers[0].public_key,
        hex::decode(&bob.peer_id).unwrap()
    );

    assert_eq!(bob_peers.len(), 1, "bob should have one peer");
    assert_eq!(bob_peers[0].peer_id, alice.peer_id);
    assert_eq!(
        bob_peers[0].public_key,
        hex::decode(&alice.peer_id).unwrap()
    );

    // Granted subjects round-tripped.
    assert_eq!(
        alice_peers[0].granted_subjects,
        vec!["/work/**".to_string()]
    );
    assert_eq!(bob_peers[0].granted_subjects, vec!["/work/**".to_string()]);
}

#[tokio::test]
async fn handshake_rejected_when_auto_accept_deny() {
    let alice = Daemon::start_memory(AutoAcceptPolicy::Any).await;
    let bob = Daemon::start_memory(AutoAcceptPolicy::Deny).await;

    let result = alice
        .fed
        .handshake_outbound(
            &bob.peer_id,
            &bob.addr.to_string(),
            &["/work/**".to_string()],
        )
        .await;

    assert!(
        result.is_err(),
        "handshake must fail when remote denies auto-accept"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("denied") || err.contains("rejected"),
        "error should mention denial: {err}"
    );
}

#[tokio::test]
async fn handshake_accepted_when_pubkey_in_allowlist() {
    let alice = Daemon::start_memory(AutoAcceptPolicy::Any).await;
    let mut allow = std::collections::HashSet::new();
    allow.insert(alice.peer_id.clone());
    let bob = Daemon::start_memory(AutoAcceptPolicy::Allowlist(allow)).await;

    let enrolled = alice
        .dial_and_handshake(&bob, &["/work/**".to_string()])
        .await;
    assert_eq!(hex::encode(&enrolled.remote_pubkey), bob.peer_id);
}
