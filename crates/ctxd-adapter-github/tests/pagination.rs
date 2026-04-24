//! Pagination via the `Link: rel="next"` header.

mod common;

use ctxd_adapter_github::config::{RepoRef, ResourceKind};
use ctxd_adapter_github::GitHubAdapter;
use serde_json::json;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn issue(num: i64) -> serde_json::Value {
    json!({
        "number": num,
        "title": format!("issue-{num}"),
        "body": "",
        "state": "open",
        "user": { "login": "u", "id": 1, "type": "User" },
        "labels": [],
        "assignees": [],
        "milestone": null,
        "created_at": "2026-04-01T00:00:00Z",
        "updated_at": format!("2026-04-10T00:00:0{num}Z"),
        "closed_at": null,
        "html_url": "",
    })
}

#[tokio::test]
async fn follows_link_next_through_two_pages() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();

    let page2_url = format!("{}/repos/acme/web/issues?page=2", server.uri());

    // Page 1: returns issues 1,2 + Link header pointing at page 2.
    Mock::given(method("GET"))
        .and(path("/repos/acme/web/issues"))
        .and(query_param("state", "all"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!([issue(1), issue(2)]))
                .insert_header(
                    "link",
                    format!("<{page2_url}>; rel=\"next\", <{page2_url}>; rel=\"last\""),
                ),
        )
        .mount(&server)
        .await;

    // Page 2: no Link header → terminate pagination.
    Mock::given(method("GET"))
        .and(path("/repos/acme/web/issues"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([issue(3), issue(4)])))
        .mount(&server)
        .await;

    // Empty placeholders for the other endpoints.
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
    let adapter = GitHubAdapter::new(cfg);
    let (sink, events) = common::CollectingSink::new();
    adapter.run_once(&sink).await.unwrap();

    let evs = events.lock().await.clone();
    let nums: Vec<i64> = evs
        .iter()
        .map(|e| e.data["number"].as_i64().unwrap())
        .collect();
    nums.iter().for_each(|n| assert!([1, 2, 3, 4].contains(n)));
    assert_eq!(
        nums.len(),
        4,
        "expected 4 issues across 2 pages, got {evs:?}"
    );
}
