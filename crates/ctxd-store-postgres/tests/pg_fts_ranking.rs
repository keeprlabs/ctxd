//! Known-answer FTS test.
//!
//! Insert three events with overlapping vocabulary and assert that the
//! ranking returned by `search` puts the most-relevant document first.

use ctxd_core::event::Event;
use ctxd_core::subject::Subject;
use ctxd_store_postgres::PostgresStore;
use sqlx::Executor;

fn pg_url_or_skip(test_name: &str) -> Option<String> {
    match std::env::var("CTXD_PG_URL") {
        Ok(url) => Some(url),
        Err(_) => {
            eprintln!("[{test_name}] CTXD_PG_URL unset — skipping");
            None
        }
    }
}

async fn fresh_store() -> PostgresStore {
    let url = std::env::var("CTXD_PG_URL").expect("CTXD_PG_URL");
    let admin = sqlx::PgPool::connect(&url).await.expect("admin connect");
    let schema = format!("ctxd_test_{}", uuid::Uuid::now_v7().simple());
    admin
        .execute(format!("CREATE SCHEMA \"{schema}\"").as_str())
        .await
        .expect("create schema");
    drop(admin);
    let scoped = if url.contains('?') {
        format!("{url}&options=-c%20search_path%3D{schema}")
    } else {
        format!("{url}?options=-c%20search_path%3D{schema}")
    };
    PostgresStore::connect(&scoped).await.expect("open store")
}

#[tokio::test]
async fn fts_ranks_dense_match_first() {
    if pg_url_or_skip("fts_ranks_dense_match_first").is_none() {
        return;
    }
    let store = fresh_store().await;

    // Three events under different subjects, with the term "database"
    // appearing once, three times, and zero times respectively. The
    // event with the highest density should rank first.
    let mk = |subj: &str, content: serde_json::Value| {
        Event::new(
            "ctxd://test".to_string(),
            Subject::new(subj).unwrap(),
            "demo".to_string(),
            content,
        )
    };
    store
        .append(mk(
            "/fts/sparse",
            serde_json::json!({"text": "we mention database briefly here"}),
        ))
        .await
        .unwrap();
    store
        .append(mk(
            "/fts/dense",
            serde_json::json!({
                "text": "database database database — repeated mentions of the database term"
            }),
        ))
        .await
        .unwrap();
    store
        .append(mk(
            "/fts/none",
            serde_json::json!({"text": "this document is about something else entirely"}),
        ))
        .await
        .unwrap();

    let hits = store.search("database", None).await.expect("search");
    assert_eq!(hits.len(), 2, "only two events match the term");

    // Dense match should outrank sparse.
    assert_eq!(
        hits[0].subject.as_str(),
        "/fts/dense",
        "expected /fts/dense first, got {:?}",
        hits.iter().map(|e| e.subject.as_str()).collect::<Vec<_>>()
    );
    assert_eq!(hits[1].subject.as_str(), "/fts/sparse");
}

#[tokio::test]
async fn fts_respects_limit() {
    if pg_url_or_skip("fts_respects_limit").is_none() {
        return;
    }
    let store = fresh_store().await;

    for i in 0..5 {
        let subj = Subject::new(&format!("/fts/limit/{i}")).unwrap();
        let evt = Event::new(
            "ctxd://test".to_string(),
            subj,
            "demo".to_string(),
            serde_json::json!({"text": "limit term limit term"}),
        );
        store.append(evt).await.unwrap();
    }

    let hits = store.search("limit", Some(2)).await.expect("search");
    assert_eq!(hits.len(), 2);
}
