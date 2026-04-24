//! Shared test harness for federation integration tests.
//!
//! Each test in this directory wires up two-or-more in-process ctxd
//! daemons against ephemeral TCP ports and exercises the federation
//! protocol. The harness centralizes:
//!
//! - SQLite store creation (in-memory by default).
//! - Local Ed25519 signing key + matching peer-id derivation.
//! - Spawning a `ProtocolServer` with a `PeerManager` attached.
//! - A handful of helpers that wrap raw `peer add`-style calls.

use ctxd_cap::CapEngine;
use ctxd_cli::federation::{AutoAcceptPolicy, EnrolledPeer, PeerManager};
use ctxd_cli::protocol::ProtocolServer;
use ctxd_core::signing::EventSigner;
use ctxd_store::EventStore;
use std::net::SocketAddr;
use std::sync::Arc;
use tempfile::TempDir;

/// A single in-process daemon harness: store + cap engine + signer +
/// PeerManager + server task.
#[allow(dead_code)]
pub struct Daemon {
    /// SQLite-backed event store.
    pub store: Arc<EventStore>,
    /// Capability engine (root key).
    pub cap_engine: Arc<CapEngine>,
    /// Local peer id (pubkey hex).
    pub peer_id: String,
    /// Federation manager.
    pub fed: Arc<PeerManager>,
    /// Bound TCP address of this daemon's wire protocol.
    pub addr: SocketAddr,
    /// Ed25519 signing key bytes (32 bytes).
    pub signing_key: Vec<u8>,
    /// Server task — kept around so the daemon stays up until drop.
    _server_handle: tokio::task::JoinHandle<()>,
    /// Replication broadcast task — kept alive for the daemon's lifetime.
    _replication_handle: tokio::task::JoinHandle<()>,
    /// Tempdir holding the on-disk SQLite file (when not in-memory).
    pub _tempdir: Option<TempDir>,
}

impl Daemon {
    /// Build an in-memory daemon with the given auto-accept policy.
    pub async fn start_memory(policy: AutoAcceptPolicy) -> Self {
        Self::start(policy, None).await
    }

    /// Build a daemon. When `db_path` is None, uses an in-memory store.
    pub async fn start(policy: AutoAcceptPolicy, db_path: Option<std::path::PathBuf>) -> Self {
        let (store, tempdir) = match db_path {
            Some(p) => (EventStore::open(&p).await.expect("open"), None),
            None => (EventStore::open_memory().await.expect("memory"), None),
        };
        let store = Arc::new(store);
        let cap_engine = Arc::new(CapEngine::new());

        // Local signing key: persist into the store so a restart can
        // recover the same identity.
        let signing_bytes = match store.get_metadata("signing_key").await.expect("meta") {
            Some(b) => b,
            None => {
                let s = EventSigner::new();
                store
                    .set_metadata("signing_key", &s.secret_key_bytes())
                    .await
                    .expect("persist");
                store
                    .set_metadata("signing_public_key", &s.public_key_bytes())
                    .await
                    .expect("persist");
                s.secret_key_bytes()
            }
        };
        let signer = EventSigner::from_bytes(&signing_bytes).expect("signer");
        let peer_id = hex::encode(signer.public_key_bytes());

        // Bind ephemeral TCP for the wire protocol — keep the listener
        // alive and pass it to ProtocolServer::run_with_listener so we
        // never release the port.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local_addr");

        let store_for_server = (*store).clone();
        let server = ProtocolServer::new(store_for_server, cap_engine.clone(), addr);
        let event_tx = server.event_sender();
        let fed = Arc::new(PeerManager::new(
            store.clone(),
            cap_engine.clone(),
            peer_id.clone(),
            signing_bytes.clone(),
            event_tx,
            policy,
        ));
        let server = server.with_federation(fed.clone());

        let _server_handle = tokio::spawn(async move {
            if let Err(e) = server.run_with_listener(listener).await {
                eprintln!("test daemon server exited: {e}");
            }
        });

        // Give the server a moment to start accepting. 20ms is plenty on localhost.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        let _replication_handle = fed.start_replication_tasks();

        Self {
            store,
            cap_engine,
            peer_id,
            fed,
            addr,
            signing_key: signing_bytes,
            _server_handle,
            _replication_handle,
            _tempdir: tempdir,
        }
    }

    /// Convenience: dial the other daemon and complete the handshake,
    /// granting `subjects` to it.
    pub async fn dial_and_handshake(&self, other: &Daemon, subjects: &[String]) -> EnrolledPeer {
        self.fed
            .handshake_outbound(&other.peer_id, &other.addr.to_string(), subjects)
            .await
            .expect("handshake")
    }
}
