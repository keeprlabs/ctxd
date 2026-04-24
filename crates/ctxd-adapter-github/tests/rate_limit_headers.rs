//! Adapter pauses when X-RateLimit-Remaining drops below threshold.

mod common;

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ctxd_adapter_github::config::{RepoRef, ResourceKind};
use ctxd_adapter_github::GitHubAdapter;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn pauses_until_rate_limit_reset() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();

    // Reset 1 second from now (low enough we can measure the pause without
    // making the test slow).
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let reset = now + 1;

    Mock::given(method("GET"))
        .and(path("/repos/acme/web/issues"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!([]))
                .insert_header("x-ratelimit-limit", "100")
                .insert_header("x-ratelimit-remaining", "1")
                .insert_header("x-ratelimit-reset", reset.to_string().as_str()),
        )
        .mount(&server)
        .await;

    // Empty placeholders (also low-quota — but we only assert the *first* path).
    for p in [
        "/repos/acme/web/pulls",
        "/repos/acme/web/issues/comments",
        "/repos/acme/web/pulls/comments",
    ] {
        Mock::given(method("GET"))
            .and(path(p))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!([]))
                    .insert_header("x-ratelimit-limit", "100")
                    .insert_header("x-ratelimit-remaining", "5000")
                    .insert_header("x-ratelimit-reset", "0"),
            )
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
    let (sink, _events) = common::CollectingSink::new();

    let start = Instant::now();
    adapter.run_once(&sink).await.unwrap();
    let elapsed = start.elapsed();

    // We expect at least ~1s wait (allow generous floor for CI variability).
    assert!(
        elapsed >= Duration::from_millis(800),
        "expected rate-limit pause >=800ms, got {:?}",
        elapsed
    );
    // Sanity ceiling so a regression that hangs forever fails fast.
    assert!(elapsed < Duration::from_secs(10));
}
