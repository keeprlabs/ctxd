//! PR lifecycle: opened → updated → merged across three poll cycles.

mod common;

use ctxd_adapter_github::config::{RepoRef, ResourceKind};
use ctxd_adapter_github::GitHubAdapter;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn pr(num: i64, updated: &str, state: &str, merged: bool) -> serde_json::Value {
    json!({
        "number": num,
        "title": "feat: add foo",
        "body": "small body",
        "state": state,
        "user": { "login": "bob", "id": 2, "type": "User" },
        "labels": [],
        "assignees": [],
        "milestone": null,
        "created_at": "2026-04-01T00:00:00Z",
        "updated_at": updated,
        "closed_at": if state == "closed" { json!("2026-04-12T00:00:00Z") } else { json!(null) },
        "merged": merged,
        "merged_at": if merged { json!("2026-04-12T00:00:00Z") } else { json!(null) },
        "merge_commit_sha": if merged { json!("abc123") } else { json!(null) },
        "head": { "ref": "feat-foo", "sha": "deadbeef" },
        "base": { "ref": "main", "sha": "0000ff" },
        "html_url": format!("https://github.com/acme/web/pull/{num}"),
    })
}

#[tokio::test]
async fn pr_lifecycle_three_cycles_share_sink() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();

    for p in [
        "/repos/acme/web/issues",
        "/repos/acme/web/issues/comments",
        "/repos/acme/web/pulls/comments",
    ] {
        Mock::given(method("GET"))
            .and(path(p))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&server)
            .await;
    }

    Mock::given(method("GET"))
        .and(path("/repos/acme/web/pulls"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([pr(
            7,
            "2026-04-10T10:00:00Z",
            "open",
            false
        )])))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/web/pulls"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([pr(
            7,
            "2026-04-10T12:00:00Z",
            "open",
            false
        )])))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/web/pulls"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([pr(
            7,
            "2026-04-12T00:00:00Z",
            "closed",
            true
        )])))
        .mount(&server)
        .await;

    let cfg = common::explicit_config(
        &server.uri(),
        dir.path().to_path_buf(),
        vec![RepoRef {
            owner: "acme".into(),
            name: "web".into(),
        }],
        vec![ResourceKind::Pulls],
        false,
    );

    let (sink, events) = common::CollectingSink::new();
    for _ in 0..3 {
        let adapter = GitHubAdapter::new(cfg.clone());
        adapter.run_once(&sink).await.expect("poll ok");
    }

    let evs: Vec<_> = events.lock().await.clone();
    let types: Vec<&str> = evs.iter().map(|e| e.event_type.as_str()).collect();
    assert_eq!(
        types,
        vec!["pr.opened", "pr.updated", "pr.merged"],
        "got {evs:?}"
    );
    for e in &evs {
        assert_eq!(e.subject, "/work/github/acme/web/pulls/7");
    }
}
