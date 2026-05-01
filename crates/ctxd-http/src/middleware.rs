//! Global middleware applied to every ctxd HTTP response: host-header
//! validation (DNS-rebinding defense) and defensive response headers
//! (CSP, X-Content-Type-Options, X-Frame-Options, Referrer-Policy).
//!
//! Applied as tower layers in `build_router`. Both layers run in front
//! of every route, including `/health`.

use axum::body::Body;
use axum::extract::Request;
use axum::http::{header, HeaderName, HeaderValue, Response, StatusCode};
use axum::middleware::Next;
use axum::response::IntoResponse;

/// Default Host header values accepted by [`host_check`] when the
/// daemon binds 127.0.0.1:7777. Composed in order of expected
/// frequency.
pub const DEFAULT_ALLOWED_HOSTS: &[&str] = &["127.0.0.1:7777", "localhost:7777", "[::1]:7777"];

/// Reject any request whose `Host:` header is not in the configured
/// allow-list with `421 Misdirected Request`. This is the primary
/// defense against DNS rebinding: even when the TCP peer is loopback,
/// a malicious site that re-resolves its hostname to 127.0.0.1 still
/// sends `Host: evil.com` (browsers preserve the requested origin in
/// the Host header), so the rejection lands.
///
/// Returns a closure that captures the allow-list. Apply with
/// `Router::layer(axum::middleware::from_fn_with_state(allowed,
/// host_check))` if you want to thread state, or use
/// [`make_host_check_layer`] for the common "fixed list" case.
pub async fn host_check(
    axum::extract::State(allowed): axum::extract::State<std::sync::Arc<Vec<String>>>,
    req: Request,
    next: Next,
) -> Response<Body> {
    // Empty allow-list = check disabled. Used by integration tests that
    // construct requests via `tower::oneshot` (no Host header) and by
    // call sites that intentionally accept any Host.
    if allowed.is_empty() {
        return next.run(req).await;
    }
    if let Some(host) = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
    {
        // Case-insensitive compare on the host portion only — the
        // Host header is canonically lowercase but some clients send
        // uppercase, and ports are case-irrelevant anyway.
        let host_lower = host.to_ascii_lowercase();
        let ok = allowed.iter().any(|a| a.eq_ignore_ascii_case(&host_lower));
        if ok {
            return next.run(req).await;
        }
    }
    (StatusCode::MISDIRECTED_REQUEST, "host header not allowed").into_response()
}

/// Apply the host-check layer to a router with a fixed allow-list.
/// Wrapper around `axum::middleware::from_fn_with_state` that hides
/// the verbose closure and `Arc` plumbing at the call site.
pub fn apply_host_check<S>(router: axum::Router<S>, allowed: Vec<String>) -> axum::Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    let allowed = std::sync::Arc::new(allowed);
    router.layer(axum::middleware::from_fn_with_state(allowed, host_check))
}

/// Set a small set of safe-by-default response headers on every
/// response. Cheap, pure, no allocation per request beyond the static
/// header values. Intended to ride at the outermost layer so it covers
/// even error responses from earlier middleware (host_check etc).
pub async fn defensive_headers(req: Request, next: Next) -> Response<Body> {
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();

    // Each set_if_absent call leaves any handler-provided override in
    // place — important for endpoints that legitimately need a
    // different policy (e.g. a future OAuth callback).
    fn set_if_absent(h: &mut axum::http::HeaderMap, name: HeaderName, value: HeaderValue) {
        h.entry(name).or_insert(value);
    }

    set_if_absent(
        h,
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self'; style-src 'self'; \
             img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'",
        ),
    );
    set_if_absent(
        h,
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    set_if_absent(
        h,
        HeaderName::from_static("x-frame-options"),
        HeaderValue::from_static("DENY"),
    );
    set_if_absent(
        h,
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use axum::Router;
    use http_body_util::BodyExt;
    use tower::util::ServiceExt;

    async fn ok_handler() -> &'static str {
        "ok"
    }

    fn router_with_host_check(allowed: Vec<&'static str>) -> Router {
        apply_host_check(
            Router::new().route("/", get(ok_handler)),
            allowed.into_iter().map(String::from).collect(),
        )
    }

    #[tokio::test]
    async fn host_check_allows_127() {
        let r = router_with_host_check(vec!["127.0.0.1:7777"]);
        let resp = r
            .oneshot(
                Request::builder()
                    .uri("/")
                    .header(header::HOST, "127.0.0.1:7777")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn host_check_allows_localhost() {
        let r = router_with_host_check(vec!["127.0.0.1:7777", "localhost:7777"]);
        let resp = r
            .oneshot(
                Request::builder()
                    .uri("/")
                    .header(header::HOST, "localhost:7777")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn host_check_rejects_evil() {
        let r = router_with_host_check(vec!["127.0.0.1:7777"]);
        let resp = r
            .oneshot(
                Request::builder()
                    .uri("/")
                    .header(header::HOST, "evil.com")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::MISDIRECTED_REQUEST);
    }

    #[tokio::test]
    async fn host_check_rejects_suffix_attack() {
        // "127.0.0.1.evil.com" is NOT 127.0.0.1:7777 — must not slip
        // through a substring match.
        let r = router_with_host_check(vec!["127.0.0.1:7777"]);
        let resp = r
            .oneshot(
                Request::builder()
                    .uri("/")
                    .header(header::HOST, "127.0.0.1.evil.com")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::MISDIRECTED_REQUEST);
    }

    #[tokio::test]
    async fn host_check_rejects_missing_host() {
        // Hyper-style request without Host should be rejected.
        let r = router_with_host_check(vec!["127.0.0.1:7777"]);
        let resp = r
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::MISDIRECTED_REQUEST);
    }

    #[tokio::test]
    async fn defensive_headers_set_on_responses() {
        let r = Router::new()
            .route("/", get(ok_handler))
            .layer(axum::middleware::from_fn(defensive_headers));
        let resp = r
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(resp
            .headers()
            .get(header::CONTENT_SECURITY_POLICY)
            .is_some());
        assert_eq!(
            resp.headers().get("x-content-type-options").unwrap(),
            "nosniff"
        );
        assert_eq!(resp.headers().get("x-frame-options").unwrap(), "DENY");
        assert_eq!(
            resp.headers().get(header::REFERRER_POLICY).unwrap(),
            "no-referrer"
        );

        // Drain body to satisfy `unused` lint.
        let _ = resp.into_body().collect().await.unwrap();
    }
}
