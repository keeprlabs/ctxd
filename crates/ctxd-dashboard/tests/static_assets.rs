//! Integration tests for the dashboard's static-asset handler.
//! Loopback middleware is exercised in step 6's real-TCP tests
//! (crates/ctxd-cli/tests/dashboard_serve.rs) — `tower::oneshot`
//! doesn't populate `ConnectInfo<SocketAddr>`, so middleware tests
//! against it would be testing the wrong layer.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use tower::util::ServiceExt;

#[tokio::test]
async fn root_serves_index_html() {
    let router: axum::Router<()> = ctxd_dashboard::router();
    let resp = router
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get(header::CONTENT_TYPE).unwrap(),
        "text/html; charset=utf-8"
    );
    assert_eq!(
        resp.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-cache"
    );
    assert!(resp.headers().get(header::ETAG).is_some());
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let body_str = std::str::from_utf8(&body).unwrap();
    assert!(body_str.starts_with("<!doctype html>"));
}

#[tokio::test]
async fn static_css_has_correct_mime() {
    let router: axum::Router<()> = ctxd_dashboard::router();
    let resp = router
        .oneshot(
            Request::builder()
                .uri("/static/style.css")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get(header::CONTENT_TYPE).unwrap(),
        "text/css; charset=utf-8"
    );
}

#[tokio::test]
async fn static_js_has_correct_mime() {
    let router: axum::Router<()> = ctxd_dashboard::router();
    let resp = router
        .oneshot(
            Request::builder()
                .uri("/static/app.js")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get(header::CONTENT_TYPE).unwrap(),
        "application/javascript; charset=utf-8"
    );
}

#[tokio::test]
async fn static_svg_has_correct_mime() {
    let router: axum::Router<()> = ctxd_dashboard::router();
    let resp = router
        .oneshot(
            Request::builder()
                .uri("/static/favicon.svg")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get(header::CONTENT_TYPE).unwrap(),
        "image/svg+xml"
    );
}

#[tokio::test]
async fn static_missing_returns_404() {
    let router: axum::Router<()> = ctxd_dashboard::router();
    let resp = router
        .oneshot(
            Request::builder()
                .uri("/static/no-such-file.css")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn static_path_traversal_returns_400() {
    let router: axum::Router<()> = ctxd_dashboard::router();
    let resp = router
        .oneshot(
            Request::builder()
                .uri("/static/..%2F..%2Fetc%2Fpasswd")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}
