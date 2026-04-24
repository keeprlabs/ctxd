//! Verifies the PAT, API-version, and User-Agent headers are sent on every request.

mod common;

use ctxd_adapter_github::config::{RepoRef, ResourceKind};
use ctxd_adapter_github::GitHubAdapter;
use wiremock::matchers::{header, header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn every_request_carries_required_headers() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();

    // Match every request: requires the four headers.
    Mock::given(method("GET"))
        .and(header("Authorization", "Bearer test-token"))
        .and(header("X-GitHub-Api-Version", "2022-11-28"))
        .and(header_exists("User-Agent"))
        .and(header_exists("Accept"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&server)
        .await;

    // Catch-all that fails the test if a request slips past the matchers.
    Mock::given(method("GET"))
        .and(path("/never"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let cfg = common::explicit_config(
        &server.uri(),
        dir.path().to_path_buf(),
        vec![RepoRef {
            owner: "acme".into(),
            name: "web".into(),
        }],
        vec![
            ResourceKind::Issues,
            ResourceKind::Pulls,
            ResourceKind::Comments,
        ],
        false,
    );
    let adapter = GitHubAdapter::new(cfg);
    let (sink, _events) = common::CollectingSink::new();
    adapter.run_once(&sink).await.expect("poll cycle ok");

    // Confirm we made requests against the expected paths (not /never).
    let received = server.received_requests().await.expect("requests recorded");
    assert!(
        !received.is_empty(),
        "expected at least one request to the mock"
    );
    for r in &received {
        assert!(
            r.headers.get("authorization").is_some(),
            "missing auth header on {}",
            r.url
        );
        assert_eq!(
            r.headers.get("authorization").and_then(|v| v.to_str().ok()),
            Some("Bearer test-token")
        );
        assert_eq!(
            r.headers
                .get("x-github-api-version")
                .and_then(|v| v.to_str().ok()),
            Some("2022-11-28")
        );
        let ua = r
            .headers
            .get("user-agent")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            ua.contains("ctxd-adapter-github/0.3"),
            "user-agent missing or wrong: {ua}"
        );
    }
}
