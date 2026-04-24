//! Issue comments + PR review comments — both publish under the right subjects.

mod common;

use ctxd_adapter_github::config::{RepoRef, ResourceKind};
use ctxd_adapter_github::GitHubAdapter;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn issue_and_pr_comments_publish_with_correct_subjects() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();

    // Issues + pulls placeholders.
    for p in ["/repos/acme/web/issues", "/repos/acme/web/pulls"] {
        Mock::given(method("GET"))
            .and(path(p))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&server)
            .await;
    }

    let issue_comment = json!({
        "id": 1001,
        "body": "Looks good",
        "user": { "login": "alice", "id": 1, "type": "User" },
        "created_at": "2026-04-10T00:00:00Z",
        "updated_at": "2026-04-10T00:00:00Z",
        "html_url": "https://github.com/acme/web/issues/42#issuecomment-1001",
        "issue_url": "https://api.github.com/repos/acme/web/issues/42",
    });

    let pr_comment = json!({
        "id": 2002,
        "body": "Tiny nit",
        "user": { "login": "bob", "id": 2, "type": "User" },
        "created_at": "2026-04-10T00:30:00Z",
        "updated_at": "2026-04-10T00:30:00Z",
        "html_url": "https://github.com/acme/web/pull/7#discussion_r2002",
        "pull_request_url": "https://api.github.com/repos/acme/web/pulls/7",
    });

    Mock::given(method("GET"))
        .and(path("/repos/acme/web/issues/comments"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([issue_comment])))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/repos/acme/web/pulls/comments"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([pr_comment])))
        .mount(&server)
        .await;

    let cfg = common::explicit_config(
        &server.uri(),
        dir.path().to_path_buf(),
        vec![RepoRef {
            owner: "acme".into(),
            name: "web".into(),
        }],
        vec![ResourceKind::Comments],
        false,
    );
    let adapter = GitHubAdapter::new(cfg);
    let (sink, events) = common::CollectingSink::new();
    adapter.run_once(&sink).await.unwrap();

    let evs = events.lock().await.clone();
    assert_eq!(evs.len(), 2, "got {evs:?}");

    let icmt = evs
        .iter()
        .find(|e| e.subject == "/work/github/acme/web/issues/42/comments/1001")
        .expect("issue comment subject");
    assert_eq!(icmt.event_type, "comment.created");
    assert_eq!(icmt.data["parent_kind"], "issue");
    assert_eq!(icmt.data["parent_number"], 42);

    let pcmt = evs
        .iter()
        .find(|e| e.subject == "/work/github/acme/web/pulls/7/comments/2002")
        .expect("pr comment subject");
    assert_eq!(pcmt.event_type, "comment.created");
    assert_eq!(pcmt.data["parent_kind"], "pull_request");
    assert_eq!(pcmt.data["parent_number"], 7);
}
