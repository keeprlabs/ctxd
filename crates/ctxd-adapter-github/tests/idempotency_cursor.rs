//! Restarting the adapter on the same state-dir does not re-publish events.

mod common;

use ctxd_adapter_github::config::{RepoRef, ResourceKind};
use ctxd_adapter_github::GitHubAdapter;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn issue(num: i64, updated: &str) -> serde_json::Value {
    json!({
        "number": num,
        "title": "x",
        "body": "",
        "state": "open",
        "user": { "login": "u", "id": 1, "type": "User" },
        "labels": [],
        "assignees": [],
        "milestone": null,
        "created_at": "2026-04-01T00:00:00Z",
        "updated_at": updated,
        "closed_at": null,
        "html_url": "",
    })
}

#[tokio::test]
async fn restart_does_not_republish_seen_resources() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();

    // Empty placeholders.
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

    // Both polls return the same data — same updated_at.
    Mock::given(method("GET"))
        .and(path("/repos/acme/web/issues"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            issue(1, "2026-04-10T00:00:00Z"),
            issue(2, "2026-04-10T01:00:00Z"),
        ])))
        .mount(&server)
        .await;

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

    // First "process": publish 2 issues.
    {
        let adapter = GitHubAdapter::new(cfg.clone());
        let (sink, events) = common::CollectingSink::new();
        adapter.run_once(&sink).await.unwrap();
        assert_eq!(events.lock().await.len(), 2);
    }

    // "Restart" — same state-dir, fresh adapter + sink.
    {
        let adapter = GitHubAdapter::new(cfg);
        let (sink, events) = common::CollectingSink::new();
        adapter.run_once(&sink).await.unwrap();
        let evs = events.lock().await.clone();
        assert_eq!(
            evs.len(),
            0,
            "restart re-published events; cursor not respected: {evs:?}"
        );
    }
}
