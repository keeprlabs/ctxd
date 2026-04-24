//! 403 with Retry-After (secondary rate limit) is honored.

mod common;

use std::time::{Duration, Instant};

use ctxd_adapter_github::config::{RepoRef, ResourceKind};
use ctxd_adapter_github::GitHubAdapter;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn retries_after_secondary_rate_limit() {
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

    // First call: 403 with retry-after: 2 + remaining=0.
    Mock::given(method("GET"))
        .and(path("/repos/acme/web/issues"))
        .respond_with(
            ResponseTemplate::new(403)
                .insert_header("retry-after", "2")
                .insert_header("x-ratelimit-remaining", "0")
                .insert_header("x-ratelimit-limit", "5000")
                .insert_header("x-ratelimit-reset", "0")
                .set_body_string("rate limited"),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;

    // Second call (after retry): success.
    Mock::given(method("GET"))
        .and(path("/repos/acme/web/issues"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
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
    let adapter = GitHubAdapter::new(cfg);
    let (sink, _events) = common::CollectingSink::new();

    let start = Instant::now();
    adapter.run_once(&sink).await.unwrap();
    let elapsed = start.elapsed();

    // Should take at least ~2s (the retry-after value).
    assert!(
        elapsed >= Duration::from_millis(1800),
        "expected retry pause ≥1.8s, got {:?}",
        elapsed
    );
    assert!(elapsed < Duration::from_secs(10));

    // Confirm we made 2 hits to /issues.
    let recv = server.received_requests().await.unwrap();
    let issues_calls = recv
        .iter()
        .filter(|r| r.url.path() == "/repos/acme/web/issues")
        .count();
    assert!(
        issues_calls >= 2,
        "expected ≥2 calls to /issues, got {issues_calls}"
    );
}
