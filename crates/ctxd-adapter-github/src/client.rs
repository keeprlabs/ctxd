//! HTTP client that talks to the GitHub REST API.
//!
//! Responsibilities:
//!
//! - Adds `Authorization: Bearer …`, `X-GitHub-Api-Version`, and `User-Agent`
//!   on every request.
//! - Persists ETags via [`crate::state::StateDb`] and replays them as
//!   `If-None-Match`. A `304 Not Modified` short-circuits to
//!   [`Page::not_modified()`].
//! - Honors `X-RateLimit-Remaining` / `X-RateLimit-Reset`: pauses the caller
//!   when the remaining quota drops below 10% of the limit until the reset
//!   timestamp.
//! - On `429` / secondary-rate-limit (`403` with `Retry-After`), sleeps for
//!   `Retry-After` and retries once.
//! - On `5xx`, retries with exponential backoff + jitter (deterministic in
//!   tests because the jitter range is bounded and the base is small).
//! - Follows pagination via the `Link: …; rel="next"` header through
//!   [`Self::fetch_all`].

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::header::{HeaderMap, HeaderName, HeaderValue, AUTHORIZATION, USER_AGENT};
use reqwest::{Client, Response, StatusCode};
use serde_json::Value;
use tracing::{debug, warn};

use crate::parse::{next_link, retry_after};
use crate::state::StateDb;
use crate::{GITHUB_API_VERSION, USER_AGENT as UA_VALUE};

/// Errors from the GitHub HTTP client.
#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    /// reqwest network/parse error.
    #[error("http error: {0}")]
    Reqwest(#[from] reqwest::Error),

    /// State-DB error.
    #[error("state error: {0}")]
    State(#[from] crate::state::StateError),

    /// The API returned a hard error status we don't retry on.
    #[error("github api error: {status}: {body}")]
    Status {
        /// HTTP status code.
        status: StatusCode,
        /// Response body for diagnostics.
        body: String,
    },

    /// Header value was not valid UTF-8 / valid header.
    #[error("invalid header: {0}")]
    InvalidHeader(String),

    /// Cancelled.
    #[error("cancelled")]
    Cancelled,
}

/// Rate-limit metadata pulled off a response.
#[derive(Debug, Clone, Copy, Default)]
pub struct RateLimit {
    /// Total requests permitted in this window.
    pub limit: u64,
    /// Requests remaining in this window.
    pub remaining: u64,
    /// Unix epoch seconds when the window resets.
    pub reset_unix: u64,
}

/// One page returned from [`GhClient::fetch_one`].
#[derive(Debug)]
pub struct Page {
    /// Status code (200 or 304 only — others surface as `Err`).
    pub status: StatusCode,
    /// Response body parsed as JSON. `None` on 304.
    pub body: Option<Value>,
    /// `Link` header value for pagination.
    pub link: Option<String>,
    /// `ETag` we saved (for diagnostics).
    pub etag: Option<String>,
    /// Rate-limit snapshot.
    pub rate: RateLimit,
}

impl Page {
    /// Returns true if the response was 304 Not Modified.
    pub fn not_modified(&self) -> bool {
        self.status == StatusCode::NOT_MODIFIED
    }
}

/// HTTP client with auth, ETag, and rate-limit support.
pub struct GhClient {
    inner: Client,
    base: String,
    token: String,
    state: Arc<StateDb>,
    /// Maximum number of retries on 5xx.
    max_retries: u32,
    /// Base backoff (multiplied by 2^attempt). Tests can shrink this.
    backoff_base: Duration,
}

impl GhClient {
    /// Build a new client with the given API base URL and PAT.
    pub fn new(base: String, token: String, state: Arc<StateDb>) -> Result<Self, HttpError> {
        let inner = Client::builder()
            .user_agent(UA_VALUE)
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self {
            inner,
            base,
            token,
            state,
            max_retries: 3,
            backoff_base: Duration::from_millis(250),
        })
    }

    /// Override the max number of 5xx retries (default: 3). Tests use 0.
    pub fn with_max_retries(mut self, n: u32) -> Self {
        self.max_retries = n;
        self
    }

    /// Override the base backoff (default: 250ms).
    pub fn with_backoff_base(mut self, d: Duration) -> Self {
        self.backoff_base = d;
        self
    }

    /// Build standard headers (auth + version + accept).
    fn base_headers(&self) -> Result<HeaderMap, HttpError> {
        let mut h = HeaderMap::new();
        let token_value = format!("Bearer {}", self.token);
        let mut auth = HeaderValue::from_str(&token_value)
            .map_err(|e| HttpError::InvalidHeader(format!("auth: {e}")))?;
        auth.set_sensitive(true);
        h.insert(AUTHORIZATION, auth);
        h.insert(
            HeaderName::from_static("x-github-api-version"),
            HeaderValue::from_static(GITHUB_API_VERSION),
        );
        h.insert(USER_AGENT, HeaderValue::from_static(UA_VALUE));
        h.insert(
            reqwest::header::ACCEPT,
            HeaderValue::from_static("application/vnd.github+json"),
        );
        Ok(h)
    }

    /// Resolve a URL: if it already starts with `http://` or `https://`, use
    /// it as-is (used when following pagination links). Otherwise treat as a
    /// relative path and join to the base.
    fn resolve(&self, path_or_url: &str) -> String {
        if path_or_url.starts_with("http://") || path_or_url.starts_with("https://") {
            path_or_url.to_string()
        } else if let Some(stripped) = path_or_url.strip_prefix('/') {
            format!("{}/{}", self.base.trim_end_matches('/'), stripped)
        } else {
            format!("{}/{}", self.base.trim_end_matches('/'), path_or_url)
        }
    }

    /// Pull rate-limit headers off a response.
    fn extract_rate(resp: &Response) -> RateLimit {
        let h = resp.headers();
        let parse = |name: &str| -> u64 {
            h.get(name)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse().ok())
                .unwrap_or(0)
        };
        RateLimit {
            limit: parse("x-ratelimit-limit"),
            remaining: parse("x-ratelimit-remaining"),
            reset_unix: parse("x-ratelimit-reset"),
        }
    }

    /// Pause if the rate-limit window says we should before making the next call.
    ///
    /// Strategy: if `remaining < max(1, 10% of limit)` AND a `reset_unix` is in the
    /// future, sleep until reset. Otherwise return immediately.
    pub async fn maybe_wait_for_rate_limit(rate: RateLimit) {
        if rate.limit == 0 || rate.reset_unix == 0 {
            return;
        }
        let threshold = (rate.limit / 10).max(1);
        if rate.remaining > threshold {
            return;
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if rate.reset_unix > now {
            let wait = Duration::from_secs(rate.reset_unix - now + 1);
            warn!(
                remaining = rate.remaining,
                limit = rate.limit,
                wait_secs = wait.as_secs(),
                "rate limit low; sleeping until reset"
            );
            tokio::time::sleep(wait).await;
        }
    }

    /// Strip the query string from a URL so the ETag is keyed by logical
    /// endpoint, not by the (constantly-changing) `since=` cursor.
    fn etag_key(url: &str) -> &str {
        match url.find('?') {
            Some(idx) => &url[..idx],
            None => url,
        }
    }

    /// Fetch a single page (no link-header following).
    ///
    /// On 304, the returned [`Page`] has `body: None`.
    pub async fn fetch_one(&self, path_or_url: &str) -> Result<Page, HttpError> {
        let url = self.resolve(path_or_url);
        let mut attempt: u32 = 0;
        loop {
            let mut headers = self.base_headers()?;

            // Lookup any saved ETag.
            let key = Self::etag_key(&url).to_string();
            if let Some(etag) = self.state.get_etag(&key).await? {
                if let Ok(v) = HeaderValue::from_str(&etag) {
                    headers.insert(reqwest::header::IF_NONE_MATCH, v);
                }
            }

            debug!(url, attempt, "GET");
            let resp = match self.inner.get(&url).headers(headers).send().await {
                Ok(r) => r,
                Err(e) => {
                    if attempt < self.max_retries && e.is_timeout() {
                        let backoff = self.compute_backoff(attempt);
                        warn!(?e, attempt, "request timeout; backing off");
                        tokio::time::sleep(backoff).await;
                        attempt += 1;
                        continue;
                    }
                    return Err(HttpError::Reqwest(e));
                }
            };
            let rate = Self::extract_rate(&resp);
            let status = resp.status();
            let etag_now = resp
                .headers()
                .get(reqwest::header::ETAG)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());
            let link = resp
                .headers()
                .get(reqwest::header::LINK)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());

            // 304 — short-circuit.
            if status == StatusCode::NOT_MODIFIED {
                return Ok(Page {
                    status,
                    body: None,
                    link,
                    etag: etag_now,
                    rate,
                });
            }

            // Secondary rate limit / 429.
            if status == StatusCode::TOO_MANY_REQUESTS
                || (status == StatusCode::FORBIDDEN
                    && rate.remaining == 0
                    && resp.headers().get("retry-after").is_some())
            {
                let ra = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok());
                let wait = retry_after(ra).unwrap_or(Duration::from_secs(60));
                warn!(?wait, "secondary rate limit hit, sleeping");
                tokio::time::sleep(wait).await;
                if attempt < self.max_retries {
                    attempt += 1;
                    continue;
                }
                return Err(HttpError::Status {
                    status,
                    body: String::new(),
                });
            }

            // 5xx — retry with exp backoff.
            if status.is_server_error() && attempt < self.max_retries {
                let backoff = self.compute_backoff(attempt);
                warn!(%status, ?backoff, "server error, backing off");
                tokio::time::sleep(backoff).await;
                attempt += 1;
                continue;
            }

            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                return Err(HttpError::Status { status, body });
            }

            // 200 — parse body, save ETag (keyed by URL path only).
            if let Some(etag) = &etag_now {
                if let Err(e) = self.state.put_etag(&key, etag).await {
                    warn!(?e, "failed to persist etag");
                }
            }

            let body: Value = resp.json().await?;
            return Ok(Page {
                status,
                body: Some(body),
                link,
                etag: etag_now,
                rate,
            });
        }
    }

    /// Fetch a paginated endpoint, calling `on_page` for each non-304 page in
    /// order. Stops when the `Link: …; rel="next"` header is missing.
    ///
    /// The first page is fetched at `path_or_url`. Subsequent pages use the
    /// fully-qualified URL from the link header, which lets the mock server
    /// in tests respond at a different path for page 2.
    pub async fn fetch_all<F>(
        &self,
        path_or_url: &str,
        mut on_page: F,
    ) -> Result<RateLimit, HttpError>
    where
        F: FnMut(Value) -> Result<(), HttpError>,
    {
        let mut url = path_or_url.to_string();
        let mut last_rate;
        loop {
            let page = self.fetch_one(&url).await?;
            last_rate = page.rate;
            Self::maybe_wait_for_rate_limit(page.rate).await;
            if page.not_modified() {
                debug!(url, "304; stopping pagination");
                return Ok(last_rate);
            }
            if let Some(body) = page.body {
                on_page(body)?;
            }
            match next_link(page.link.as_deref()) {
                Some(next) => {
                    url = next;
                }
                None => return Ok(last_rate),
            }
        }
    }

    /// Compute exponential backoff for retry attempt `n`. Capped at 30s.
    fn compute_backoff(&self, n: u32) -> Duration {
        let base = self.backoff_base.as_millis() as u64;
        let mult = 1u64 << n.min(6);
        let raw = base.saturating_mul(mult);
        // Add up to 25% jitter using a simple deterministic-but-spread source.
        let jitter = (raw / 4).max(1);
        let nanos_now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as u64)
            .unwrap_or(0);
        let j = nanos_now % jitter;
        Duration::from_millis(raw + j).min(Duration::from_secs(30))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::StateDb;
    use std::sync::Arc;

    #[tokio::test]
    async fn maybe_wait_skips_when_quota_healthy() {
        let r = RateLimit {
            limit: 5000,
            remaining: 4500,
            reset_unix: 999_999_999_999,
        };
        let start = std::time::Instant::now();
        GhClient::maybe_wait_for_rate_limit(r).await;
        assert!(start.elapsed() < Duration::from_millis(50));
    }

    #[tokio::test]
    async fn maybe_wait_pauses_when_low() {
        // Reset 1 second from now, remaining < 10% of limit.
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let r = RateLimit {
            limit: 100,
            remaining: 1,
            reset_unix: now + 1,
        };
        let start = std::time::Instant::now();
        GhClient::maybe_wait_for_rate_limit(r).await;
        let elapsed = start.elapsed();
        assert!(elapsed >= Duration::from_millis(900));
        assert!(elapsed < Duration::from_secs(5));
    }

    #[test]
    fn etag_key_strips_query() {
        assert_eq!(
            GhClient::etag_key("https://api.github.com/x?since=now"),
            "https://api.github.com/x"
        );
        assert_eq!(
            GhClient::etag_key("https://api.github.com/x"),
            "https://api.github.com/x"
        );
    }

    #[tokio::test]
    async fn resolve_handles_relative_and_absolute() {
        let dir = tempfile::tempdir().unwrap();
        let state = Arc::new(StateDb::open(dir.path()).await.unwrap());
        let c = GhClient::new(
            "https://api.github.com".to_string(),
            "tk".to_string(),
            state,
        )
        .unwrap();
        assert_eq!(c.resolve("/user"), "https://api.github.com/user");
        assert_eq!(
            c.resolve("https://api.github.com/x?page=2"),
            "https://api.github.com/x?page=2"
        );
        assert_eq!(c.resolve("user"), "https://api.github.com/user");
    }
}
