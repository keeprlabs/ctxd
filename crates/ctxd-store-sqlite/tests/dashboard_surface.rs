//! Tests for the dashboard inherent-impl surface added in v0.4:
//! event_count, vector_embedding_count, event_by_id, subject_counts,
//! read_paginated, search_with_snippets, subscribe.
//!
//! These methods live on `EventStore` (not the `Store` trait), so the
//! tests target the SQLite store directly via `open_memory()`.

use ctxd_core::event::Event;
use ctxd_core::subject::Subject;
use ctxd_store_sqlite::EventStore;

/// Helper: append a fresh `ctx.note` event under `subject`. Returns
/// the stored event with predecessor hash + seq populated by the store.
async fn append(store: &EventStore, subject: &str, body: &str) -> Event {
    let subj = Subject::new(subject).unwrap();
    let event = Event::new(
        "ctxd://test".to_string(),
        subj,
        "ctx.note".to_string(),
        serde_json::json!({"content": body}),
    );
    store.append(event).await.unwrap()
}

#[tokio::test]
async fn event_count_zero_then_grows() {
    let store = EventStore::open_memory().await.unwrap();
    assert_eq!(store.event_count().await.unwrap(), 0);
    append(&store, "/x", "one").await;
    append(&store, "/x", "two").await;
    append(&store, "/y", "three").await;
    assert_eq!(store.event_count().await.unwrap(), 3);
}

#[tokio::test]
async fn vector_embedding_count_zero_when_no_embedder() {
    // No embedder is installed → no vectors are written → count is 0.
    let store = EventStore::open_memory().await.unwrap();
    append(&store, "/x", "hello world").await;
    assert_eq!(store.vector_embedding_count().await.unwrap(), 0);
}

#[tokio::test]
async fn event_by_id_hit_and_miss() {
    let store = EventStore::open_memory().await.unwrap();
    let stored = append(&store, "/me/preferences", "morning person").await;

    let got = store.event_by_id(stored.id).await.unwrap();
    assert!(got.is_some());
    let got = got.unwrap();
    assert_eq!(got.id, stored.id);
    assert_eq!(got.subject.as_str(), "/me/preferences");
    assert_eq!(got.event_type, "ctx.note");

    let missing = uuid::Uuid::nil();
    assert!(store.event_by_id(missing).await.unwrap().is_none());
}

#[tokio::test]
async fn subject_counts_groups_correctly() {
    let store = EventStore::open_memory().await.unwrap();
    append(&store, "/work/local/files/a.md", "alpha").await;
    append(&store, "/work/local/files/a.md", "alpha 2").await;
    append(&store, "/work/local/files/b.md", "bravo").await;
    append(&store, "/me/preferences", "p").await;

    let all = store.subject_counts(None).await.unwrap();
    let map: std::collections::HashMap<String, u64> = all
        .iter()
        .map(|(s, n)| (s.as_str().to_string(), *n))
        .collect();
    assert_eq!(map.get("/work/local/files/a.md").copied(), Some(2));
    assert_eq!(map.get("/work/local/files/b.md").copied(), Some(1));
    assert_eq!(map.get("/me/preferences").copied(), Some(1));
    assert_eq!(map.len(), 3);
}

#[tokio::test]
async fn subject_counts_under_prefix_does_not_match_sibling_prefix() {
    // The eng review's load-bearing test: the LIKE pattern for prefix
    // `/work` must use `'/work/%'`, NOT `'work%'`. A subject like
    // `/workshop` shares the prefix `work` but is NOT under `/work`.
    let store = EventStore::open_memory().await.unwrap();
    append(&store, "/work/notes/standup.md", "wn").await;
    append(&store, "/workshop/event.md", "ws").await;
    append(&store, "/work", "w").await;

    let prefix = Subject::new("/work").unwrap();
    let counts = store.subject_counts(Some(&prefix)).await.unwrap();
    let subjects: Vec<String> = counts.iter().map(|(s, _)| s.as_str().to_string()).collect();

    assert!(subjects.contains(&"/work".to_string()));
    assert!(subjects.contains(&"/work/notes/standup.md".to_string()));
    assert!(
        !subjects.contains(&"/workshop/event.md".to_string()),
        "sibling-prefix /workshop must not be counted under /work"
    );
}

#[tokio::test]
async fn read_paginated_newest_first_and_cursor_round_trip() {
    let store = EventStore::open_memory().await.unwrap();
    // Insert 5 events; insertion order is the seq order.
    let mut ids = Vec::new();
    for i in 0..5 {
        let ev = append(&store, "/log", &format!("event {i}")).await;
        ids.push(ev.id);
    }

    // Page 1: most recent 2 — should be event 4 then event 3.
    let page1 = store.read_paginated(None, None, 2, false).await.unwrap();
    assert_eq!(page1.len(), 2);
    assert_eq!(page1[0].1.id, ids[4]);
    assert_eq!(page1[1].1.id, ids[3]);

    // Cursor from the last item of page 1 → page 2 starts at event 2.
    let cursor = page1.last().unwrap().0;
    let page2 = store
        .read_paginated(None, Some(cursor), 2, false)
        .await
        .unwrap();
    assert_eq!(page2.len(), 2);
    assert_eq!(page2[0].1.id, ids[2]);
    assert_eq!(page2[1].1.id, ids[1]);

    // Final page: only event 0 left.
    let cursor = page2.last().unwrap().0;
    let page3 = store
        .read_paginated(None, Some(cursor), 2, false)
        .await
        .unwrap();
    assert_eq!(page3.len(), 1);
    assert_eq!(page3[0].1.id, ids[0]);

    // Past end: empty.
    let cursor = page3.last().unwrap().0;
    let page4 = store
        .read_paginated(None, Some(cursor), 2, false)
        .await
        .unwrap();
    assert!(page4.is_empty());
}

#[tokio::test]
async fn read_paginated_subject_filter_with_recursion() {
    let store = EventStore::open_memory().await.unwrap();
    append(&store, "/work/notes/a", "a").await;
    append(&store, "/work/notes/b", "b").await;
    append(&store, "/me", "m").await;

    let work = Subject::new("/work").unwrap();
    let rows = store
        .read_paginated(Some(&work), None, 10, true)
        .await
        .unwrap();
    let subs: Vec<String> = rows
        .iter()
        .map(|(_, e)| e.subject.as_str().to_string())
        .collect();
    assert_eq!(subs.len(), 2);
    assert!(subs.iter().all(|s| s.starts_with("/work/")));
}

#[tokio::test]
async fn search_with_snippets_returns_snippet_and_rank() {
    let store = EventStore::open_memory().await.unwrap();
    append(&store, "/notes/auth", "this is the authentication flow").await;
    append(&store, "/notes/billing", "monthly invoice").await;
    append(&store, "/notes/auth2", "auth tokens are scoped to subjects").await;

    let hits = store.search_with_snippets("auth", 10).await.unwrap();
    assert!(!hits.is_empty(), "expected at least one match");
    // Every hit should have a non-empty snippet that mentions the term
    // or its FTS-tokenized form.
    for h in &hits {
        assert!(!h.snippet.is_empty(), "snippet must be non-empty");
        // bm25 returns negative-ish floats; we don't assert the sign,
        // just that ordering is non-decreasing (best first).
    }
    // Best-match-first: ranks must be non-decreasing.
    for w in hits.windows(2) {
        assert!(
            w[0].rank <= w[1].rank,
            "results must be sorted by bm25 ASC (best first); got {} then {}",
            w[0].rank,
            w[1].rank
        );
    }
}

#[tokio::test]
async fn search_with_snippets_zero_matches_returns_empty() {
    let store = EventStore::open_memory().await.unwrap();
    append(&store, "/x", "alpha").await;
    let hits = store.search_with_snippets("nosuchterm", 10).await.unwrap();
    assert!(hits.is_empty());
}

#[tokio::test]
async fn subscribe_receives_appended_events() {
    let store = EventStore::open_memory().await.unwrap();
    let mut rx = store.subscribe(None);

    // Append from another task so the receive can run concurrently.
    let store_clone = store.clone();
    let writer = tokio::spawn(async move {
        append(&store_clone, "/live/tail", "first").await;
        append(&store_clone, "/live/tail", "second").await;
    });

    let first = tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv())
        .await
        .expect("first event should arrive within 500ms")
        .expect("first event recv ok");
    assert_eq!(first.subject.as_str(), "/live/tail");

    let second = tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv())
        .await
        .expect("second event should arrive within 500ms")
        .expect("second event recv ok");
    assert_eq!(second.subject.as_str(), "/live/tail");

    writer.await.unwrap();
}

#[tokio::test]
async fn subscribe_with_no_receivers_is_a_no_op_for_append() {
    // Append must succeed even when no one is subscribed. The broadcast
    // SendError when the channel has zero receivers must NOT propagate
    // up as a store error.
    let store = EventStore::open_memory().await.unwrap();
    let result = store
        .append(Event::new(
            "ctxd://test".to_string(),
            Subject::new("/silent").unwrap(),
            "ctx.note".to_string(),
            serde_json::json!({"content": "no one listening"}),
        ))
        .await;
    assert!(result.is_ok());
}
