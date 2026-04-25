//! Shared conformance test suite for [`Store`](super::Store) implementations.
//!
//! Every backend crate runs these from its own tests, passing in a factory
//! function that constructs a fresh store instance. Example:
//!
//! ```ignore
//! #[tokio::test]
//! async fn conformance() {
//!     ctxd_store_core::testsuite::run_all(|| async {
//!         MySqliteStore::open_memory().await.unwrap()
//!     })
//!     .await;
//! }
//! ```
//!
//! Adding a new [`Store`](super::Store) trait method requires adding at
//! least one conformance test here. The test suite is exhaustive by
//! design: if a backend rewires an underlying operation, we should catch
//! behavioral drift here before anything else.

use super::*;
use chrono::TimeZone;
use ctxd_core::event::Event;
use ctxd_core::subject::Subject;

/// Run a closure over a freshly-created store. Every conformance test
/// calls the factory once; stores are not reused between tests so flaky
/// shared-state bugs surface as test-level failures, not suite-level.
pub async fn with_store<Factory, Fut, S, F, FFut, Out>(factory: Factory, body: F) -> Out
where
    Factory: FnOnce() -> Fut,
    Fut: std::future::Future<Output = S>,
    S: Store,
    F: FnOnce(S) -> FFut,
    FFut: std::future::Future<Output = Out>,
{
    let store = factory().await;
    body(store).await
}

/// `append` then `read` returns the stored event with fields populated.
pub async fn append_and_read_roundtrip<S: Store>(store: S) {
    let subject = Subject::new("/conformance/append").unwrap();
    let event = Event::new(
        "ctxd://test".to_string(),
        subject.clone(),
        "demo".to_string(),
        serde_json::json!({"msg": "hi"}),
    );
    let stored = store.append(event).await.unwrap();
    assert!(
        stored.predecessorhash.is_none(),
        "first event has no predecessor"
    );

    let events = store.read(&subject, false).await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].id, stored.id);
    assert_eq!(events[0].data, serde_json::json!({"msg": "hi"}));
}

/// Sequential appends on the same subject form a hash chain.
pub async fn hash_chain_builds_across_appends<S: Store>(store: S) {
    let subject = Subject::new("/conformance/chain").unwrap();
    let e1 = Event::new(
        "ctxd://test".to_string(),
        subject.clone(),
        "demo".to_string(),
        serde_json::json!({"step": 1}),
    );
    let s1 = store.append(e1).await.unwrap();
    assert!(s1.predecessorhash.is_none());

    let e2 = Event::new(
        "ctxd://test".to_string(),
        subject.clone(),
        "demo".to_string(),
        serde_json::json!({"step": 2}),
    );
    let s2 = store.append(e2).await.unwrap();
    assert!(
        s2.predecessorhash.is_some(),
        "second event must have predecessorhash"
    );
}

/// `subjects` returns all distinct subjects under a prefix.
pub async fn subjects_listing_recursive<S: Store>(store: S) {
    for path in ["/r/a", "/r/b", "/r/a/x", "/other"] {
        let e = Event::new(
            "ctxd://test".to_string(),
            Subject::new(path).unwrap(),
            "demo".to_string(),
            serde_json::json!({}),
        );
        store.append(e).await.unwrap();
    }
    let r = Subject::new("/r").unwrap();
    let mut under = store.subjects(Some(&r), true).await.unwrap();
    under.sort();
    assert_eq!(
        under,
        vec!["/r/a".to_string(), "/r/a/x".to_string(), "/r/b".to_string()]
    );

    let only_r = store.subjects(Some(&r), false).await.unwrap();
    assert!(only_r.iter().all(|s| s == "/r"));
}

/// `search` finds matching events via FTS.
pub async fn search_finds_events<S: Store>(store: S) {
    let e1 = Event::new(
        "ctxd://test".to_string(),
        Subject::new("/search/a").unwrap(),
        "demo".to_string(),
        serde_json::json!({"content": "hello world document"}),
    );
    store.append(e1).await.unwrap();

    let e2 = Event::new(
        "ctxd://test".to_string(),
        Subject::new("/search/b").unwrap(),
        "demo".to_string(),
        serde_json::json!({"content": "unrelated material"}),
    );
    store.append(e2).await.unwrap();

    let hits = store.search("hello", None).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].subject.as_str(), "/search/a");
}

/// `kv_get` returns the latest value for a subject.
pub async fn kv_latest_value<S: Store>(store: S) {
    let s = Subject::new("/kv/x").unwrap();
    for i in 0..3 {
        let e = Event::new(
            "ctxd://test".to_string(),
            s.clone(),
            "demo".to_string(),
            serde_json::json!({"v": i}),
        );
        store.append(e).await.unwrap();
    }
    let got = store.kv_get("/kv/x").await.unwrap().unwrap();
    assert_eq!(got, serde_json::json!({"v": 2}));

    let missing = store.kv_get("/kv/nope").await.unwrap();
    assert!(missing.is_none());
}

/// `read_at` / `kv_get_at` enforce the time bound.
pub async fn temporal_read_respects_bound<S: Store>(store: S) {
    let subject = Subject::new("/temporal/bound").unwrap();
    let t1 = Utc.with_ymd_and_hms(2025, 1, 1, 10, 0, 0).unwrap();
    let t2 = Utc.with_ymd_and_hms(2025, 1, 1, 11, 0, 0).unwrap();
    let t3 = Utc.with_ymd_and_hms(2025, 1, 1, 12, 0, 0).unwrap();

    for (i, t) in [(1, t1), (2, t2), (3, t3)] {
        let mut e = Event::new(
            "ctxd://test".to_string(),
            subject.clone(),
            "demo".to_string(),
            serde_json::json!({"v": i}),
        );
        e.time = t;
        store.append(e).await.unwrap();
    }
    let at_t2 = store.read_at(&subject, t2, false).await.unwrap();
    assert_eq!(at_t2.len(), 2);

    let val_at_t2 = store
        .kv_get_at("/temporal/bound", t2)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(val_at_t2, serde_json::json!({"v": 2}));

    let val_at_t1 = store
        .kv_get_at("/temporal/bound", t1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(val_at_t1, serde_json::json!({"v": 1}));
}

/// Peer registration round-trip.
pub async fn peer_register_and_list<S: Store>(store: S) {
    let peer = Peer {
        peer_id: "peer-1".to_string(),
        url: "tcp://localhost:5432".to_string(),
        public_key: vec![1u8; 32],
        granted_subjects: vec!["/work/**".to_string()],
        trust_level: serde_json::json!({"auto_accept": false}),
        added_at: Utc::now(),
    };
    store.peer_add(peer.clone()).await.unwrap();

    let list = store.peer_list().await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].peer_id, "peer-1");
    assert_eq!(list[0].granted_subjects, vec!["/work/**".to_string()]);

    // Idempotent on re-add.
    store.peer_add(peer.clone()).await.unwrap();
    assert_eq!(store.peer_list().await.unwrap().len(), 1);

    store.peer_remove("peer-1").await.unwrap();
    assert!(store.peer_list().await.unwrap().is_empty());

    // Removing a missing peer is not an error.
    store.peer_remove("nope").await.unwrap();
}

/// Peer cursor round-trip.
pub async fn peer_cursor_roundtrip<S: Store>(store: S) {
    let peer = Peer {
        peer_id: "peer-a".to_string(),
        url: "tcp://a".to_string(),
        public_key: vec![2u8; 32],
        granted_subjects: vec!["/a/**".to_string()],
        trust_level: serde_json::json!({}),
        added_at: Utc::now(),
    };
    store.peer_add(peer).await.unwrap();

    let cursor = PeerCursor {
        peer_id: "peer-a".to_string(),
        subject_pattern: "/a/**".to_string(),
        last_event_id: Some(uuid::Uuid::now_v7()),
        last_event_time: Some(Utc::now()),
    };
    store.peer_cursor_set(cursor.clone()).await.unwrap();

    let fetched = store
        .peer_cursor_get("peer-a", "/a/**")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fetched.last_event_id, cursor.last_event_id);

    let missing = store.peer_cursor_get("peer-a", "/other").await.unwrap();
    assert!(missing.is_none());
}

/// Token revocation round-trip.
pub async fn token_revocation_roundtrip<S: Store>(store: S) {
    assert!(!store.is_token_revoked("tok-1").await.unwrap());
    store.revoke_token("tok-1").await.unwrap();
    assert!(store.is_token_revoked("tok-1").await.unwrap());

    // Idempotent.
    store.revoke_token("tok-1").await.unwrap();
    assert!(store.is_token_revoked("tok-1").await.unwrap());
}

/// Vector upsert + search round-trip for backends that support vector search.
/// Backends that don't support vector search should return an empty vec from
/// `vector_search`, which this test explicitly tolerates.
pub async fn vector_upsert_and_search<S: Store>(store: S) {
    let subject = Subject::new("/vec/a").unwrap();
    let e = Event::new(
        "ctxd://test".to_string(),
        subject,
        "demo".to_string(),
        serde_json::json!({}),
    );
    let stored = store.append(e).await.unwrap();

    store
        .vector_upsert(&stored.id.to_string(), "test-model", &[1.0, 0.0, 0.0])
        .await
        .unwrap();

    let results = store.vector_search(&[1.0, 0.0, 0.0], 1).await.unwrap();
    // Backends without vector support may return empty; that is allowed.
    if !results.is_empty() {
        assert_eq!(results[0].event_id, stored.id.to_string());
    }
}

/// Parents and attestation survive append + read.
pub async fn parents_and_attestation_survive_append<S: Store>(store: S) {
    let subject = Subject::new("/merge/conformance").unwrap();
    let p1 = uuid::Uuid::parse_str("00000000-0000-7000-8000-00000000000a").unwrap();
    let p2 = uuid::Uuid::parse_str("00000000-0000-7000-8000-00000000000b").unwrap();

    let mut e = Event::new(
        "ctxd://test".to_string(),
        subject.clone(),
        "demo".to_string(),
        serde_json::json!({"merge": true}),
    );
    e.parents = vec![p2, p1]; // intentionally unsorted
    e.attestation = Some(vec![0xfe, 0xed]);

    store.append(e).await.unwrap();
    let out = store.read(&subject, false).await.unwrap();
    assert_eq!(out.len(), 1);
    // Canonical ordering: parents should come back sorted.
    assert_eq!(out[0].parents, vec![p1, p2]);
    assert_eq!(out[0].attestation.as_deref(), Some(&[0xfe, 0xedu8][..]));
}

/// Aggregate runner — every conformance test runs against a fresh store.
///
/// The `factory` produces an independent store each invocation.
pub async fn run_all<Factory, Fut, S>(factory: Factory)
where
    Factory: Fn() -> Fut + Clone,
    Fut: std::future::Future<Output = S>,
    S: Store,
{
    with_store(factory.clone(), append_and_read_roundtrip).await;
    with_store(factory.clone(), hash_chain_builds_across_appends).await;
    with_store(factory.clone(), subjects_listing_recursive).await;
    with_store(factory.clone(), search_finds_events).await;
    with_store(factory.clone(), kv_latest_value).await;
    with_store(factory.clone(), temporal_read_respects_bound).await;
    with_store(factory.clone(), peer_register_and_list).await;
    with_store(factory.clone(), peer_cursor_roundtrip).await;
    with_store(factory.clone(), token_revocation_roundtrip).await;
    with_store(factory.clone(), vector_upsert_and_search).await;
    with_store(factory.clone(), parents_and_attestation_survive_append).await;
}
