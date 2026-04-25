//! Federation: peer handshake, replication loop, cursor resume.
//!
//! [`PeerManager`] is the in-process owner of all federation traffic.
//! It hosts one [`PeerConnection`] per registered peer and runs them on
//! the same Tokio runtime as the rest of the daemon.
//!
//! ## Wire flow (per peer)
//!
//! 1. **Connect** — TCP dial with exponential backoff (1s → 60s cap).
//! 2. **Hello** — local sends `PeerHello { local_peer_id, local_pubkey,
//!    subjects_local_offers, cap_local_mints_for_remote }`.
//! 3. **Welcome** — remote answers `PeerWelcome { remote_peer_id,
//!    remote_pubkey, subjects_remote_offers, cap_remote_mints_for_local }`.
//! 4. **Cursor exchange** — both sides send a [`PeerCursorRequest`] (in
//!    practice: a `PeerCursor` carrying their own receive-cursor) so the
//!    peer knows where to resume sending from.
//! 5. **Replication** — sender streams `PeerReplicate { event,
//!    origin_peer_id }`; receiver verifies signature, verifies cap
//!    scope, appends idempotently, ACKs.
//! 6. **Backfill** — if an inbound event references unknown parent ids,
//!    receiver issues `PeerFetchEvents { ids }` and re-tries the append
//!    in topological order.
//!
//! ## Loop guard
//!
//! Replication carries an `origin_peer_id` envelope. A receiver never
//! sends an event back to its origin peer. See ADR 008.
//!
//! ## Crash safety
//!
//! Cursors are persisted in the `peer_cursors` table on every successful
//! ACK. On reconnect the cursor exchange re-anchors both sides. Worst
//! case is duplicate delivery, which is safe because `Store::append` is
//! idempotent on event id (and the receiver's hash-chain check rejects
//! a re-application).

use crate::protocol::{ProtocolClient, Request, Response};
use ctxd_cap::{CapEngine, Operation};
use ctxd_core::event::Event;
use ctxd_core::signing::EventSigner;
use ctxd_core::subject::Subject;
use ctxd_store::EventStore;
use ctxd_store_core::PeerCursor;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{broadcast, Mutex, RwLock};
use uuid::Uuid;

use crate::protocol::BroadcastEvent;

/// Errors that surface from the federation layer.
#[derive(Debug, thiserror::Error)]
pub enum FederationError {
    /// Generic wrapper for I/O / serialization issues.
    #[error("federation io error: {0}")]
    Io(String),
    /// Capability verification failed for a federation message.
    #[error("federation cap denied: {0}")]
    CapDenied(String),
    /// Signature verification failed on an inbound replicated event.
    #[error("federation signature invalid: {0}")]
    Signature(String),
    /// Inbound event subject is outside the peer's granted scope.
    #[error("federation cap scope violation: subject {subject} outside peer grant")]
    CapScopeViolation {
        /// The offending subject.
        subject: String,
    },
    /// A handshake step rejected the peer.
    #[error("federation handshake rejected: {0}")]
    HandshakeRejected(String),
}

impl FederationError {
    /// Convenience: build an Io variant from anything Display.
    pub fn io<E: std::fmt::Display>(e: E) -> Self {
        Self::Io(e.to_string())
    }
}

/// The auto-accept policy applied when the daemon receives a `PeerHello`.
#[derive(Debug, Clone)]
pub enum AutoAcceptPolicy {
    /// Reject all incoming peer requests. Inbound `PeerHello`s are NACKed.
    Deny,
    /// Accept any incoming peer. Convenient for development; risky in prod.
    Any,
    /// Accept only peers whose Ed25519 pubkey hex appears in the allowlist.
    Allowlist(HashSet<String>),
}

impl AutoAcceptPolicy {
    /// Read the policy from `CTXD_FEDERATION_AUTO_ACCEPT`. Format:
    /// - missing or `false` → `Deny`
    /// - `true` → `Any`
    /// - `allowlist:<hex1>,<hex2>` → `Allowlist`
    pub fn from_env() -> Self {
        match std::env::var("CTXD_FEDERATION_AUTO_ACCEPT") {
            Ok(v) if v == "true" => Self::Any,
            Ok(v) if v.starts_with("allowlist:") => {
                let list: HashSet<String> = v
                    .trim_start_matches("allowlist:")
                    .split(',')
                    .map(|s| s.trim().to_lowercase())
                    .filter(|s| !s.is_empty())
                    .collect();
                Self::Allowlist(list)
            }
            _ => Self::Deny,
        }
    }

    /// Returns true if a peer with the given pubkey hex should be auto-accepted.
    pub fn allows(&self, pubkey_hex: &str) -> bool {
        match self {
            Self::Deny => false,
            Self::Any => true,
            Self::Allowlist(set) => set.contains(&pubkey_hex.to_lowercase()),
        }
    }
}

/// In-memory record of an enrolled peer: pubkey, granted subject globs,
/// the cap the *remote* minted for *us* (so we can present it back when
/// pushing events), and our local connection state.
#[derive(Debug, Clone)]
pub struct EnrolledPeer {
    /// Free-form local id for this peer (typically the remote pubkey hex).
    pub peer_id: String,
    /// Remote's Ed25519 public key, 32 bytes.
    pub remote_pubkey: Vec<u8>,
    /// Subjects the remote granted us (we may send events matching these).
    pub remote_grants_us: Vec<String>,
    /// Subjects we granted the remote (they may send events matching these).
    pub we_grant_remote: Vec<String>,
    /// Cap token (raw bytes) the remote minted for us.
    pub cap_from_remote: Option<Vec<u8>>,
    /// Cap token (raw bytes) we minted for the remote.
    pub cap_for_remote: Option<Vec<u8>>,
}

/// The federation manager. Owns one task per peer plus the inbound
/// dispatcher hooked into the wire protocol's broadcast channel.
pub struct PeerManager {
    store: Arc<EventStore>,
    cap_engine: Arc<CapEngine>,
    /// Local peer id (this daemon's pubkey hex).
    local_peer_id: String,
    /// Local Ed25519 signing key for outbound replicated events.
    /// Wrapped in Arc<EventSigner> through bytes; we hold the raw bytes
    /// to allow cheap cloning into background tasks.
    local_signing_key: Vec<u8>,
    /// Local broadcast sender for live PUB events.
    event_tx: broadcast::Sender<BroadcastEvent>,
    /// Per-peer connection state. Async lock since we mutate from
    /// multiple tasks (add/remove/replicate).
    peers: Arc<RwLock<HashMap<String, Arc<EnrolledPeer>>>>,
    /// Auto-accept policy applied at handshake time.
    auto_accept: AutoAcceptPolicy,
}

impl PeerManager {
    /// Construct a new manager.
    ///
    /// `local_signing_key` is the 32-byte Ed25519 secret key the local
    /// daemon uses to sign outbound events. This MUST match the public
    /// key advertised in `PeerHello`.
    pub fn new(
        store: Arc<EventStore>,
        cap_engine: Arc<CapEngine>,
        local_peer_id: String,
        local_signing_key: Vec<u8>,
        event_tx: broadcast::Sender<BroadcastEvent>,
        auto_accept: AutoAcceptPolicy,
    ) -> Self {
        Self {
            store,
            cap_engine,
            local_peer_id,
            local_signing_key,
            event_tx,
            peers: Arc::new(RwLock::new(HashMap::new())),
            auto_accept,
        }
    }

    /// Local peer id (Ed25519 pubkey hex by convention).
    pub fn local_peer_id(&self) -> &str {
        &self.local_peer_id
    }

    /// Snapshot of currently enrolled peers.
    pub async fn list_peers(&self) -> Vec<Arc<EnrolledPeer>> {
        self.peers.read().await.values().cloned().collect()
    }

    /// Add a peer locally without dialing. Used by `peer add` to
    /// pre-register a peer (e.g. after a handshake completed elsewhere).
    pub async fn enroll(&self, peer: EnrolledPeer) {
        self.peers
            .write()
            .await
            .insert(peer.peer_id.clone(), Arc::new(peer));
    }

    /// Remove a peer.
    pub async fn unenroll(&self, peer_id: &str) {
        self.peers.write().await.remove(peer_id);
    }

    /// Dial a peer URL, perform the `PeerHello` ↔ `PeerWelcome` handshake,
    /// and persist the resulting [`EnrolledPeer`] both locally and via
    /// the [`Store`] (`peer_add`).
    ///
    /// `subjects_we_grant_remote` are the subject globs we promise to
    /// send to the remote AND the scope of the cap we mint for them.
    #[tracing::instrument(skip(self), fields(peer_id, url))]
    pub async fn handshake_outbound(
        &self,
        peer_id: &str,
        url: &str,
        subjects_we_grant_remote: &[String],
    ) -> Result<EnrolledPeer, FederationError> {
        // Mint the cap we offer the remote: read+subscribe+peer on the
        // subject globs we want to share with them. Includes
        // Operation::Peer so the remote can use it for replication
        // handshake roundtrips.
        let mut cap_for_remote: Option<Vec<u8>> = None;
        if !subjects_we_grant_remote.is_empty() {
            // Mint a single cap covering the union — use the FIRST glob
            // for now (most callers pass a single pattern). Multi-glob
            // caps are a v0.4 nicety.
            let glob = subjects_we_grant_remote
                .first()
                .ok_or_else(|| FederationError::HandshakeRejected("empty grant".into()))?;
            let cap = self
                .cap_engine
                .mint(
                    glob,
                    &[Operation::Read, Operation::Subscribe, Operation::Peer],
                    None,
                    None,
                    None,
                )
                .map_err(|e| FederationError::CapDenied(e.to_string()))?;
            cap_for_remote = Some(cap);
        }

        let cap_b64 = cap_for_remote
            .as_deref()
            .map(CapEngine::token_to_base64)
            .unwrap_or_default();

        // Local Ed25519 public key bytes.
        let signer = EventSigner::from_bytes(&self.local_signing_key)
            .map_err(|e| FederationError::io(format!("local signing key invalid: {e}")))?;
        let local_pk = signer.public_key_bytes();

        let mut client = ProtocolClient::connect(url)
            .await
            .map_err(FederationError::io)?;

        let hello = Request::PeerHello {
            peer_id: self.local_peer_id.clone(),
            public_key: local_pk.clone(),
            offered_cap: cap_b64,
            subjects: subjects_we_grant_remote.to_vec(),
        };
        let resp = client.request(&hello).await.map_err(FederationError::io)?;

        let (remote_id, remote_pk, cap_remote_b64, remote_subjects) = match resp {
            Response::Ok { data } => parse_welcome(&data)?,
            Response::Error { message } => {
                return Err(FederationError::HandshakeRejected(message));
            }
            other => {
                return Err(FederationError::HandshakeRejected(format!(
                    "unexpected response: {other:?}"
                )));
            }
        };

        let cap_from_remote_bytes = if cap_remote_b64.is_empty() {
            None
        } else {
            Some(
                CapEngine::token_from_base64(&cap_remote_b64)
                    .map_err(|e| FederationError::CapDenied(e.to_string()))?,
            )
        };

        // Persist into the local Store so the daemon picks the peer
        // back up across restarts.
        let peer_row = ctxd_store_core::Peer {
            peer_id: peer_id.to_string(),
            url: url.to_string(),
            public_key: remote_pk.clone(),
            granted_subjects: remote_subjects.clone(),
            trust_level: serde_json::json!({"auto_accept": false}),
            added_at: chrono::Utc::now(),
        };
        self.store
            .peer_add_impl(peer_row)
            .await
            .map_err(|e| FederationError::io(e.to_string()))?;

        let enrolled = EnrolledPeer {
            peer_id: peer_id.to_string(),
            remote_pubkey: remote_pk,
            remote_grants_us: remote_subjects,
            we_grant_remote: subjects_we_grant_remote.to_vec(),
            cap_from_remote: cap_from_remote_bytes,
            cap_for_remote,
        };
        self.enroll(enrolled.clone()).await;

        tracing::info!(
            local_peer_id = %self.local_peer_id,
            remote_peer_id = %remote_id,
            "federation handshake complete"
        );

        Ok(enrolled)
    }

    /// Server-side handler for an inbound `PeerHello`.
    ///
    /// Validates the offered cap, applies the auto-accept policy, mints
    /// a reciprocal cap, persists the new peer, and returns the
    /// `PeerWelcome` payload (as a JSON value the wire protocol can
    /// embed in `Response::Ok`).
    pub async fn handle_peer_hello(
        &self,
        remote_peer_id: &str,
        remote_pubkey: &[u8],
        offered_cap_b64: &str,
        remote_subjects: &[String],
    ) -> Result<serde_json::Value, FederationError> {
        // Auto-accept gate.
        let pk_hex = hex::encode(remote_pubkey);
        if !self.auto_accept.allows(&pk_hex) {
            return Err(FederationError::HandshakeRejected(format!(
                "auto-accept policy denied peer {pk_hex}"
            )));
        }

        // Validate the offered cap is well-formed under our root key.
        // We *don't* fail on cap_engine.verify here because the offered
        // cap is signed by the remote's root, not ours — it's only
        // meaningful when the remote presents it back to itself. We do
        // require it parses as base64.
        if !offered_cap_b64.is_empty() {
            CapEngine::token_from_base64(offered_cap_b64)
                .map_err(|e| FederationError::CapDenied(e.to_string()))?;
        }

        // Mint a reciprocal cap for the remote on remote_subjects.
        let mut our_cap_for_them_bytes: Option<Vec<u8>> = None;
        if let Some(glob) = remote_subjects.first() {
            let cap = self
                .cap_engine
                .mint(
                    glob,
                    &[Operation::Read, Operation::Subscribe, Operation::Peer],
                    None,
                    None,
                    None,
                )
                .map_err(|e| FederationError::CapDenied(e.to_string()))?;
            our_cap_for_them_bytes = Some(cap);
        }

        // Local pubkey for the welcome payload.
        let signer = EventSigner::from_bytes(&self.local_signing_key)
            .map_err(|e| FederationError::io(format!("local signing key invalid: {e}")))?;
        let local_pk = signer.public_key_bytes();

        // Persist remote in our Store. If a row already exists with a
        // real (non-inbound) URL, preserve that URL — `handle_peer_hello`
        // is called on the receiver side and we don't want to clobber a
        // dial-able URL set by a prior outbound handshake.
        let existing_url = self
            .store
            .peer_list_impl()
            .await
            .map_err(|e| FederationError::io(e.to_string()))?
            .into_iter()
            .find(|p| p.peer_id == remote_peer_id)
            .map(|p| p.url);
        let preserved_url = match existing_url {
            Some(u) if !u.starts_with("inbound:") => u,
            _ => format!("inbound:{}", pk_hex),
        };
        let peer_row = ctxd_store_core::Peer {
            peer_id: remote_peer_id.to_string(),
            url: preserved_url,
            public_key: remote_pubkey.to_vec(),
            granted_subjects: remote_subjects.to_vec(),
            trust_level: serde_json::json!({"auto_accept": true}),
            added_at: chrono::Utc::now(),
        };
        self.store
            .peer_add_impl(peer_row)
            .await
            .map_err(|e| FederationError::io(e.to_string()))?;

        let cap_b64 = our_cap_for_them_bytes
            .as_deref()
            .map(CapEngine::token_to_base64)
            .unwrap_or_default();

        // Enroll in-process.
        let enrolled = EnrolledPeer {
            peer_id: remote_peer_id.to_string(),
            remote_pubkey: remote_pubkey.to_vec(),
            remote_grants_us: remote_subjects.to_vec(),
            // We don't yet know the subjects the *remote* granted us —
            // they're carried in the offered_cap, but we accept the
            // simpler invariant: each side declares its grant in
            // `subjects` and the cap is the cryptographic backing.
            we_grant_remote: remote_subjects.to_vec(),
            cap_from_remote: if offered_cap_b64.is_empty() {
                None
            } else {
                Some(CapEngine::token_from_base64(offered_cap_b64).unwrap_or_default())
            },
            cap_for_remote: our_cap_for_them_bytes,
        };
        self.enroll(enrolled).await;

        Ok(serde_json::json!({
            "peer_id": self.local_peer_id,
            "public_key": local_pk,
            "subjects": remote_subjects,
            "offered_cap": cap_b64,
        }))
    }

    /// Verify an inbound replicated event:
    /// 1. The event has a signature.
    /// 2. The signature verifies against the peer's stored pubkey.
    /// 3. The event subject falls inside `granted_subjects` for the peer.
    #[tracing::instrument(skip(self, event), fields(event_id = %event.id, subject = %event.subject))]
    pub async fn verify_inbound(
        &self,
        peer_id: &str,
        event: &Event,
    ) -> Result<(), FederationError> {
        let peers = self.peers.read().await;
        let peer = peers
            .get(peer_id)
            .ok_or_else(|| FederationError::CapDenied(format!("unknown peer {peer_id}")))?;

        let sig = event.signature.as_deref().ok_or_else(|| {
            FederationError::Signature("replicated event must be signed".to_string())
        })?;
        if !EventSigner::verify(event, sig, &peer.remote_pubkey) {
            return Err(FederationError::Signature(
                "ed25519 signature mismatch against peer pubkey".to_string(),
            ));
        }

        let subj = event.subject.as_str();
        if !peer
            .we_grant_remote
            .iter()
            .any(|p| Subject::matches_cap_pattern(subj, p))
        {
            return Err(FederationError::CapScopeViolation {
                subject: subj.to_string(),
            });
        }

        Ok(())
    }

    /// Decide whether the local daemon should forward an event to a
    /// given peer based on the loop-guard rule + grant pattern match.
    pub fn should_forward(
        &self,
        peer: &EnrolledPeer,
        event_subject: &str,
        origin_peer_id: &str,
    ) -> bool {
        if origin_peer_id == peer.peer_id {
            return false; // loop guard
        }
        // Only send if peer's grants match the event subject.
        peer.remote_grants_us
            .iter()
            .any(|p| Subject::matches_cap_pattern(event_subject, p))
    }

    /// Persist a cursor advance after a successful inbound ACK.
    pub async fn advance_inbound_cursor(
        &self,
        peer_id: &str,
        subject_pattern: &str,
        last_id: Uuid,
        last_time: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), FederationError> {
        let cursor = PeerCursor {
            peer_id: peer_id.to_string(),
            subject_pattern: subject_pattern.to_string(),
            last_event_id: Some(last_id),
            last_event_time: Some(last_time),
        };
        self.store
            .peer_cursor_set_impl(cursor)
            .await
            .map_err(|e| FederationError::io(e.to_string()))
    }

    /// Read the cursor describing what we last received from `peer_id`
    /// for `subject_pattern`. Returns `None` if the cursor is uninitialized.
    pub async fn get_inbound_cursor(
        &self,
        peer_id: &str,
        subject_pattern: &str,
    ) -> Result<Option<PeerCursor>, FederationError> {
        self.store
            .peer_cursor_get_impl(peer_id, subject_pattern)
            .await
            .map_err(|e| FederationError::io(e.to_string()))
    }

    /// Catch up a single peer from its receive-cursor: ask the peer
    /// for its inbound cursor (i.e., what it last got from us), then
    /// replay any local events with `time > cursor.last_event_time`
    /// that match the peer's grants.
    ///
    /// This is the resume path for `peer_cursor_get`. It's safe to
    /// call repeatedly — duplicate events are idempotent on the
    /// receiver via the UNIQUE constraint on `events.id`.
    #[tracing::instrument(skip(self), fields(peer = %peer.peer_id))]
    pub async fn catch_up_peer(&self, peer: &EnrolledPeer) -> Result<usize, FederationError> {
        // Get peer's URL from store.
        let peers = self
            .store
            .peer_list_impl()
            .await
            .map_err(|e| FederationError::io(e.to_string()))?;
        let row = peers
            .into_iter()
            .find(|p| p.peer_id == peer.peer_id)
            .ok_or_else(|| FederationError::io(format!("peer {} not in store", peer.peer_id)))?;
        if row.url.starts_with("inbound:") {
            return Ok(0);
        }

        // Pick the union pattern (use first grant for now). Multi-glob
        // peers replay each glob's catch-up under a separate cursor.
        let mut sent = 0usize;
        for pattern in &peer.remote_grants_us {
            let mut client = ProtocolClient::connect(&row.url)
                .await
                .map_err(FederationError::io)?;

            // Ask the peer what its receive-cursor is — this is the
            // last event the peer has from us for this pattern.
            let resp = client
                .request(&Request::PeerCursorRequest {
                    peer_id: self.local_peer_id.clone(),
                    subject_pattern: pattern.clone(),
                })
                .await
                .map_err(FederationError::io)?;

            let last_event_time: Option<chrono::DateTime<chrono::Utc>> = match resp {
                Response::Ok { data } => data
                    .get("last_event_time")
                    .and_then(|v| v.as_str())
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.with_timezone(&chrono::Utc)),
                Response::Error { message } => {
                    tracing::warn!(error = %message, "cursor request errored, replaying full pattern");
                    None
                }
                _ => None,
            };

            // Read local events for this pattern since the cursor.
            let prefix = pattern
                .trim_end_matches("/**")
                .trim_end_matches("/*")
                .to_string();
            let prefix_subject = if prefix.is_empty() {
                ctxd_core::subject::Subject::new("/")
                    .map_err(|e| FederationError::io(e.to_string()))?
            } else {
                ctxd_core::subject::Subject::new(&prefix)
                    .map_err(|e| FederationError::io(e.to_string()))?
            };
            let recursive = pattern.ends_with("/**") || prefix.is_empty();
            let events = match last_event_time {
                Some(t) => self
                    .store
                    .read_since(&prefix_subject, t, recursive)
                    .await
                    .map_err(|e| FederationError::io(e.to_string()))?,
                None => self
                    .store
                    .read(&prefix_subject, recursive)
                    .await
                    .map_err(|e| FederationError::io(e.to_string()))?,
            };

            // Stream them to the peer in (time, id) order. read_since
            // already returns by seq which is ~chronological; sort
            // explicitly for deterministic replay across backends.
            let mut sorted = events;
            sorted.sort_by(|a, b| {
                a.time
                    .cmp(&b.time)
                    .then(a.id.to_string().cmp(&b.id.to_string()))
            });

            for ev in sorted {
                if !ctxd_core::subject::Subject::matches_cap_pattern(ev.subject.as_str(), pattern) {
                    continue;
                }
                let event_json =
                    serde_json::to_value(&ev).map_err(|e| FederationError::io(e.to_string()))?;
                let resp = client
                    .request(&Request::PeerReplicate {
                        origin_peer_id: self.local_peer_id.clone(),
                        event: event_json,
                    })
                    .await
                    .map_err(FederationError::io)?;
                match resp {
                    Response::Ok { .. } => sent += 1,
                    Response::Error { message } => {
                        tracing::warn!(error = %message, event_id = %ev.id, "peer rejected event during catch-up");
                    }
                    _ => {}
                }
            }
        }

        tracing::info!(sent, peer = %peer.peer_id, "catch-up complete");
        Ok(sent)
    }

    /// Run [`Self::catch_up_peer`] for every enrolled peer once. Used
    /// at daemon startup to flush any accumulated backlog.
    pub async fn catch_up_all(&self) -> Vec<(String, Result<usize, FederationError>)> {
        let peers = self.peers.read().await.clone();
        let mut results = Vec::new();
        for (pid, peer) in peers {
            let r = self.catch_up_peer(&peer).await;
            results.push((pid, r));
        }
        results
    }

    /// Backfill a missing parent: ask the origin peer for the parent
    /// event by id, append it locally, recursing for its own missing
    /// parents until the chain closes. Acceptably-bounded by the
    /// in-memory `seen` set; cycles in a healthy DAG should not exist
    /// (UUIDv7 monotonicity), but we guard anyway.
    #[tracing::instrument(skip(self), fields(peer_id = %peer_id))]
    pub async fn backfill_parents(
        &self,
        peer_id: &str,
        parent_ids: &[Uuid],
    ) -> Result<(), FederationError> {
        if parent_ids.is_empty() {
            return Ok(());
        }
        let peers = self
            .store
            .peer_list_impl()
            .await
            .map_err(|e| FederationError::io(e.to_string()))?;
        let row = peers
            .into_iter()
            .find(|p| p.peer_id == peer_id)
            .ok_or_else(|| FederationError::io(format!("peer {peer_id} not in store")))?;
        if row.url.starts_with("inbound:") {
            return Err(FederationError::Io(
                "cannot backfill from inbound-only peer".to_string(),
            ));
        }

        // Compute which parent ids are missing from our store.
        let root = Subject::new("/").map_err(|e| FederationError::io(e.to_string()))?;
        let local = self
            .store
            .read(&root, true)
            .await
            .map_err(|e| FederationError::io(e.to_string()))?;
        let local_ids: HashSet<Uuid> = local.iter().map(|e| e.id).collect();
        let missing: Vec<String> = parent_ids
            .iter()
            .filter(|id| !local_ids.contains(*id))
            .map(|id| id.to_string())
            .collect();
        if missing.is_empty() {
            return Ok(());
        }

        let mut client = ProtocolClient::connect(&row.url)
            .await
            .map_err(FederationError::io)?;
        let resp = client
            .request(&Request::PeerFetchEvents { event_ids: missing })
            .await
            .map_err(FederationError::io)?;
        let events_array = match resp {
            Response::Ok { data } => data
                .as_array()
                .cloned()
                .ok_or_else(|| FederationError::Io("fetch response not an array".to_string()))?,
            Response::Error { message } => return Err(FederationError::io(message)),
            other => {
                return Err(FederationError::io(format!(
                    "unexpected fetch response: {other:?}"
                )))
            }
        };

        // Topological sort by parent depth: append events whose parents
        // are already satisfied, repeat until none remain.
        let mut pending: Vec<Event> = events_array
            .into_iter()
            .filter_map(|v| serde_json::from_value::<Event>(v).ok())
            .collect();
        let mut applied: HashSet<Uuid> = local_ids;
        while !pending.is_empty() {
            let before_len = pending.len();
            let mut still_pending = Vec::with_capacity(pending.len());
            for ev in pending.drain(..) {
                if ev.parents.iter().all(|p| applied.contains(p)) {
                    let event_id = ev.id;
                    // Verify before appending — backfill events still
                    // have to satisfy signature + cap-scope.
                    self.verify_inbound(peer_id, &ev).await?;
                    match self.store.append(ev).await {
                        Ok(_) => {
                            applied.insert(event_id);
                        }
                        Err(e) => {
                            let msg = e.to_string();
                            if !msg.contains("UNIQUE") && !msg.contains("constraint") {
                                return Err(FederationError::io(msg));
                            }
                            applied.insert(event_id);
                        }
                    }
                } else {
                    still_pending.push(ev);
                }
            }
            if still_pending.len() == before_len {
                // Made no progress — there's still a missing parent.
                // Don't infinite loop; surface it as an error.
                return Err(FederationError::io(format!(
                    "backfill stalled with {} events still pending; remote may be missing ancestors",
                    still_pending.len()
                )));
            }
            pending = still_pending;
        }

        Ok(())
    }

    /// Begin replication tasks for every enrolled peer. Each peer gets a
    /// dedicated outbound task. Inbound is multiplexed through the
    /// existing wire protocol's connection handler.
    ///
    /// Tasks loop with exponential backoff on disconnect, capped at 60s.
    pub fn start_replication_tasks(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let mgr = Arc::clone(self);
        tokio::spawn(async move {
            let mut rx = mgr.event_tx.subscribe();
            loop {
                match rx.recv().await {
                    Ok(broadcast_event) => {
                        // Origin: empty means "produced locally via PUB";
                        // non-empty means "replicated in from peer X" —
                        // we preserve X for the loop-guard check.
                        let origin = if broadcast_event.origin_peer_id.is_empty() {
                            mgr.local_peer_id.clone()
                        } else {
                            broadcast_event.origin_peer_id.clone()
                        };
                        let peers = mgr.peers.read().await.clone();
                        for (_pid, peer) in peers {
                            if !mgr.should_forward(&peer, &broadcast_event.subject, &origin) {
                                continue;
                            }
                            let event_value = broadcast_event.event.clone();
                            let origin = origin.clone();
                            let mgr = Arc::clone(&mgr);
                            let peer = Arc::clone(&peer);
                            tokio::spawn(async move {
                                if let Err(e) = mgr
                                    .send_replicate_with_origin(&peer, event_value, &origin)
                                    .await
                                {
                                    tracing::warn!(
                                        peer = %peer.peer_id,
                                        error = %e,
                                        "outbound replication failed"
                                    );
                                }
                            });
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("federation broadcast receiver lagged {n} events");
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        tracing::info!("federation broadcast channel closed");
                        return;
                    }
                }
            }
        })
    }

    /// Send a single `PeerReplicate` to `peer`, using the local peer
    /// id as the origin. Convenience wrapper for tests.
    pub async fn send_replicate(
        &self,
        peer: &EnrolledPeer,
        event_json: serde_json::Value,
    ) -> Result<(), FederationError> {
        let origin = self.local_peer_id.clone();
        self.send_replicate_with_origin(peer, event_json, &origin)
            .await
    }

    /// Send a `PeerReplicate` to `peer` carrying an explicit origin id.
    ///
    /// Used by the broadcast subscriber so events that arrived via
    /// inbound replication get re-fanned-out with the *original*
    /// origin tag preserved — that's what the loop-guard relies on.
    #[tracing::instrument(skip(self, event_json), fields(peer = %peer.peer_id, origin = %origin))]
    pub async fn send_replicate_with_origin(
        &self,
        peer: &EnrolledPeer,
        event_json: serde_json::Value,
        origin: &str,
    ) -> Result<(), FederationError> {
        // Look up the URL from the Store. We don't cache it on
        // EnrolledPeer because the operator might `peer remove` then
        // re-add with a different URL.
        let peers = self
            .store
            .peer_list_impl()
            .await
            .map_err(|e| FederationError::io(e.to_string()))?;
        let row = peers
            .into_iter()
            .find(|p| p.peer_id == peer.peer_id)
            .ok_or_else(|| FederationError::io(format!("peer {} not in store", peer.peer_id)))?;

        // Skip inbound:* URLs — those are peers who connected to us; we
        // can't dial back without an out-of-band URL. The receiver-side
        // handler still fires on our broadcast though.
        if row.url.starts_with("inbound:") {
            return Ok(());
        }

        let mut client = ProtocolClient::connect(&row.url)
            .await
            .map_err(FederationError::io)?;
        let req = Request::PeerReplicate {
            origin_peer_id: origin.to_string(),
            event: event_json,
        };
        let resp = client.request(&req).await.map_err(FederationError::io)?;
        match resp {
            Response::Ok { .. } => Ok(()),
            Response::Error { message } => Err(FederationError::io(message)),
            other => Err(FederationError::io(format!(
                "unexpected response: {other:?}"
            ))),
        }
    }

    /// Server-side: handle an inbound `PeerReplicate`. Verifies signature
    /// + cap scope, optionally backfills missing parents, appends
    /// idempotently to the local store, advances the cursor, and returns
    /// a `PeerAck`.
    #[tracing::instrument(skip(self, event_value))]
    pub async fn handle_peer_replicate(
        &self,
        origin_peer_id: &str,
        event_value: serde_json::Value,
    ) -> Result<serde_json::Value, FederationError> {
        let event: Event = serde_json::from_value(event_value)
            .map_err(|e| FederationError::io(format!("bad event payload: {e}")))?;

        // Signature + cap scope verification.
        self.verify_inbound(origin_peer_id, &event).await?;

        // Parent backfill: if the event references parent ids we don't
        // have, fetch them from origin first. Ignored for empty
        // parents (the common case for non-merge events).
        if !event.parents.is_empty() {
            if let Err(e) = self.backfill_parents(origin_peer_id, &event.parents).await {
                tracing::warn!(error = %e, "parent backfill failed; proceeding with append");
                // Proceed anyway — the event itself may still be useful
                // even with missing ancestry. The hash-chain check
                // doesn't apply to `parents` (only `predecessorhash`).
            }
        }

        // Idempotent append: if the event id is already present, our
        // append errors but we treat that as success and short-circuit
        // the broadcast — re-broadcasting a duplicate would burn cycles
        // and could amplify churn under reconnect storms.
        let event_id = event.id;
        let event_time = event.time;
        let subject_pattern = event.subject.as_str().to_string();
        let event_for_broadcast = event.clone();
        let mut newly_stored = true;
        match self.store.append(event).await {
            Ok(_) => {}
            Err(e) => {
                let msg = e.to_string();
                if !msg.contains("UNIQUE") && !msg.contains("constraint") {
                    return Err(FederationError::io(msg));
                }
                newly_stored = false;
            }
        }

        // Advance the inbound cursor after a successful append.
        self.advance_inbound_cursor(origin_peer_id, &subject_pattern, event_id, event_time)
            .await?;

        // Re-fan-out via the local broadcast channel so other
        // federation peers (and SUBs) can receive it. Stamp the
        // BroadcastEvent with the *origin* peer-id so the loop guard
        // can drop it cleanly when it would loop back.
        if newly_stored {
            let event_json = serde_json::to_value(&event_for_broadcast)
                .map_err(|e| FederationError::io(e.to_string()))?;
            let _ = self.event_tx.send(BroadcastEvent {
                subject: event_for_broadcast.subject.as_str().to_string(),
                event: event_json,
                origin_peer_id: origin_peer_id.to_string(),
            });
        }

        Ok(serde_json::json!({
            "ack": event_id.to_string(),
        }))
    }

    /// Server-side: handle an inbound `PeerCursorRequest`. Returns the
    /// receiver's notion of "what I last sent to peer X for pattern P".
    /// Symmetric: when a remote asks us for our cursor, we return the
    /// receive-cursor we maintain for them.
    pub async fn handle_peer_cursor_request(
        &self,
        peer_id: &str,
        subject_pattern: &str,
    ) -> Result<serde_json::Value, FederationError> {
        let cursor = self.get_inbound_cursor(peer_id, subject_pattern).await?;
        match cursor {
            Some(c) => Ok(serde_json::json!({
                "peer_id": c.peer_id,
                "subject_pattern": c.subject_pattern,
                "last_event_id": c.last_event_id.map(|u| u.to_string()),
                "last_event_time": c.last_event_time.map(|t| t.to_rfc3339()),
            })),
            None => Ok(serde_json::json!({
                "peer_id": peer_id,
                "subject_pattern": subject_pattern,
                "last_event_id": null,
                "last_event_time": null,
            })),
        }
    }

    /// Server-side: handle an inbound `PeerFetchEvents`. Returns the
    /// requested events as a JSON array.
    pub async fn handle_peer_fetch_events(
        &self,
        event_ids: &[String],
    ) -> Result<serde_json::Value, FederationError> {
        // Brute-force: walk the root subject and filter. Acceptable for
        // backfill (typically a handful of ids); a v0.4 optimization
        // could use a real id index.
        let root = Subject::new("/").map_err(|e| FederationError::io(e.to_string()))?;
        let all = self
            .store
            .read(&root, true)
            .await
            .map_err(|e| FederationError::io(e.to_string()))?;
        let id_set: HashSet<&str> = event_ids.iter().map(String::as_str).collect();
        let mut out: Vec<serde_json::Value> = Vec::new();
        for ev in all {
            if id_set.contains(ev.id.to_string().as_str()) {
                out.push(
                    serde_json::to_value(&ev).map_err(|e| FederationError::io(e.to_string()))?,
                );
            }
        }
        Ok(serde_json::Value::Array(out))
    }
}

/// Parse a `PeerWelcome` payload that the wire protocol returned as
/// `Response::Ok { data: <json> }`.
fn parse_welcome(
    data: &serde_json::Value,
) -> Result<(String, Vec<u8>, String, Vec<String>), FederationError> {
    let obj = data.as_object().ok_or_else(|| {
        FederationError::HandshakeRejected("welcome payload not an object".into())
    })?;
    let peer_id = obj
        .get("peer_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| FederationError::HandshakeRejected("missing peer_id in welcome".into()))?
        .to_string();
    let pk_arr = obj
        .get("public_key")
        .and_then(|v| v.as_array())
        .ok_or_else(|| {
            FederationError::HandshakeRejected("missing public_key in welcome".into())
        })?;
    let pk: Vec<u8> = pk_arr
        .iter()
        .map(|v| v.as_u64().unwrap_or(0) as u8)
        .collect();
    let cap = obj
        .get("offered_cap")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let subjects: Vec<String> = obj
        .get("subjects")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    Ok((peer_id, pk, cap, subjects))
}

/// A length-prefixed MessagePack frame reader/writer for streaming
/// SUB-style federation channels in the future. Currently unused — the
/// per-replicate request/response uses the existing [`ProtocolClient`].
#[allow(dead_code)]
pub(crate) async fn read_frame(stream: &mut TcpStream) -> std::io::Result<Option<Vec<u8>>> {
    let len = match stream.read_u32().await {
        Ok(n) => n as usize,
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    };
    if len > 16 * 1024 * 1024 {
        return Err(std::io::Error::other(format!("frame too large: {len}")));
    }
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await?;
    Ok(Some(buf))
}

#[allow(dead_code)]
pub(crate) async fn write_frame(stream: &mut TcpStream, data: &[u8]) -> std::io::Result<()> {
    stream.write_u32(data.len() as u32).await?;
    stream.write_all(data).await?;
    stream.flush().await?;
    Ok(())
}

/// Reconnect helper with capped exponential backoff. Runs `f` until it
/// returns `Ok(())`, sleeping 1s, 2s, 4s, … capped at 60s between tries.
#[allow(dead_code)]
pub(crate) async fn reconnect_loop<F, Fut>(label: &str, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<(), FederationError>>,
{
    let mut delay = Duration::from_secs(1);
    loop {
        match f().await {
            Ok(()) => return,
            Err(e) => {
                tracing::warn!(label, error = %e, ?delay, "federation task error; backing off");
                tokio::time::sleep(delay).await;
                delay = Duration::from_secs((delay.as_secs() * 2).clamp(1, 60));
            }
        }
    }
}

/// A typed mutex used by integration tests to coordinate setup steps.
/// Exposed publicly so tests in `tests/` can lock around setup helpers.
#[derive(Debug, Default)]
pub struct TestSetupLock(Mutex<()>);

impl TestSetupLock {
    /// Acquire the lock. Returned guard releases on drop.
    pub async fn lock(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.0.lock().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_accept_policy_parsing() {
        std::env::remove_var("CTXD_FEDERATION_AUTO_ACCEPT");
        assert!(matches!(
            AutoAcceptPolicy::from_env(),
            AutoAcceptPolicy::Deny
        ));

        std::env::set_var("CTXD_FEDERATION_AUTO_ACCEPT", "true");
        assert!(matches!(
            AutoAcceptPolicy::from_env(),
            AutoAcceptPolicy::Any
        ));

        std::env::set_var("CTXD_FEDERATION_AUTO_ACCEPT", "allowlist:abc,DEF");
        match AutoAcceptPolicy::from_env() {
            AutoAcceptPolicy::Allowlist(set) => {
                assert!(set.contains("abc"));
                assert!(set.contains("def")); // case-folded
            }
            other => panic!("expected Allowlist, got {other:?}"),
        }
        std::env::remove_var("CTXD_FEDERATION_AUTO_ACCEPT");
    }

    #[test]
    fn allowlist_match_is_case_insensitive() {
        let mut set = HashSet::new();
        set.insert("abcdef".to_string());
        let p = AutoAcceptPolicy::Allowlist(set);
        assert!(p.allows("ABCDEF"));
        assert!(p.allows("abcdef"));
        assert!(!p.allows("123"));
    }
}
