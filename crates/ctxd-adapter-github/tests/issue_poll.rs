//! Issue polling: first poll = `issue.opened`, second poll with newer
//! `updated_at` = `issue.updated`.

mod common;

use ctxd_adapter_github::config::{RepoRef, ResourceKind};
use ctxd_adapter_github::GitHubAdapter;
use serde_json::json;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn issue(num: i64, updated: &str, state: &str) -> serde_json::Value {
    json!({
        "number": num,
        "title": format!("issue #{num}"),
        "body": "body",
        "state": state,
        "user": { "login": "alice", "id": 1, "type": "User" },
        "labels": [],
        "assignees": [],
        "milestone": null,
        "created_at": "2026-04-01T00:00:00Z",
        "updated_at": updated,
        "closed_at": null,
        "html_url": format!("https://github.com/acme/web/issues/{num}"),
    })
}

#[tokio::test]
async fn first_poll_opens_then_second_poll_updates() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();

    // First poll: 2 issues, no `since` cursor.
    Mock::given(method("GET"))
        .and(path("/repos/acme/web/issues"))
        .and(query_param("state", "all"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            issue(1, "2026-04-10T10:00:00Z", "open"),
            issue(2, "2026-04-10T11:00:00Z", "open"),
        ])))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    // Second poll: same endpoint with `since=...`. Returns 1 updated issue.
    Mock::given(method("GET"))
        .and(path("/repos/acme/web/issues"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([issue(
            1,
            "2026-04-11T10:00:00Z",
            "open"
        )])))
        .mount(&server)
        .await;

    // Catch-alls for other endpoints (return empty array) — keeps tests focused.
    for p in [
        "/repos/acme/web/pulls",
        "/repos/acme/web/issues/comments",
        "/repos/acme/web/pulls/comments",
    ] {
        Mock::given(method("GET"))
            .and(path(p))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&server)
            .await;
    }

    let cfg = common::explicit_config(
        &server.uri(),
        dir.path().to_path_buf(),
        vec![RepoRef {
            owner: "acme".into(),
            name: "web".into(),
        }],
        vec![ResourceKind::Issues],
        false,
    );

    // First cycle.
    let adapter = GitHubAdapter::new(cfg.clone());
    let (sink, events) = common::CollectingSink::new();
    adapter.run_once(&sink).await.unwrap();

    let captured = events.lock().await.clone();
    let opened: Vec<_> = captured
        .iter()
        .filter(|e| e.event_type == "issue.opened")
        .collect();
    assert_eq!(
        opened.len(),
        2,
        "expected 2 issue.opened events; got {captured:?}"
    );
    for e in &opened {
        assert!(e.subject.starts_with("/work/github/acme/web/issues/"));
    }

    // Second cycle (same state-dir → cursor persists).
    let adapter2 = GitHubAdapter::new(cfg);
    let (sink2, events2) = common::CollectingSink::new();
    adapter2.run_once(&sink2).await.unwrap();

    let updated: Vec<_> = events2
        .lock()
        .await
        .clone()
        .into_iter()
        .filter(|e| e.event_type == "issue.updated")
        .collect();
    assert_eq!(
        updated.len(),
        1,
        "expected 1 issue.updated; got {:?}",
        events2.lock().await
    );
    assert_eq!(updated[0].subject, "/work/github/acme/web/issues/1");
}
