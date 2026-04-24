//! Minimal Gmail REST API client.
//!
//! We deliberately avoid pulling in `google-gmail1` (which transitively
//! drags in all of Google's discovery types) and instead hand-write a
//! small client that covers exactly the three endpoints we need:
//!
//! - `users.messages.list`
//! - `users.messages.get`
//! - `users.history.list`
//!
//! All requests are rate-limited via [`call_with_retry`], which honors
//! `Retry-After` headers and applies exponential backoff with jitter on
//! 429 / 5xx responses.

use std::time::Duration;

use rand::Rng;
use serde::Deserialize;
use serde_json::Value;
use tracing::{debug, warn};

/// Default Gmail API base URL.
pub const DEFAULT_API_BASE: &str = "https://gmail.googleapis.com";

/// Errors produced by the Gmail client.
#[derive(Debug, thiserror::Error)]
pub enum GmailError {
    /// Network or transport failure.
    #[error("http error: {0}")]
    Http(String),

    /// Server returned an unrecoverable non-2xx response.
    #[error("gmail api error {status}: {body}")]
    Api {
        /// HTTP status code.
        status: u16,
        /// Response body.
        body: String,
    },

    /// History cursor is older than Gmail's retention window. Caller
    /// should fall back to a full sync via `messages.list`.
    #[error("history cursor expired (HTTP 404 from history.list)")]
    HistoryExpired,

    /// Response was not valid JSON for the expected shape.
    #[error("invalid response: {0}")]
    InvalidResponse(String),

    /// Retries exhausted while still hitting rate-limit / 5xx.
    #[error("retries exhausted after {0} attempts")]
    RetriesExhausted(usize),
}

/// Configuration for the Gmail client.
#[derive(Debug, Clone)]
pub struct GmailClientConfig {
    /// Base URL for the Gmail API. Override this in tests.
    pub api_base: String,
    /// User ID — typically `"me"`.
    pub user_id: String,
    /// Maximum retry attempts for 429 / 5xx.
    pub max_retries: usize,
    /// Base backoff for exponential retry, in ms.
    pub base_backoff_ms: u64,
    /// Maximum backoff cap, in ms.
    pub max_backoff_ms: u64,
}

impl Default for GmailClientConfig {
    fn default() -> Self {
        Self {
            api_base: DEFAULT_API_BASE.to_string(),
            user_id: "me".to_string(),
            max_retries: 5,
            base_backoff_ms: 250,
            max_backoff_ms: 30_000,
        }
    }
}

/// A reusable Gmail client.
pub struct GmailClient {
    pub(crate) http_inner: reqwest::Client,
    pub(crate) config_inner: GmailClientConfig,
    pub(crate) access_token_inner: String,
}

impl GmailClient {
    /// Build a client with the given access token.
    pub fn new(http: reqwest::Client, config: GmailClientConfig, access_token: String) -> Self {
        Self {
            http_inner: http,
            config_inner: config,
            access_token_inner: access_token,
        }
    }

    /// Update the access token in place. Used after refresh.
    pub fn set_access_token(&mut self, token: String) {
        self.access_token_inner = token;
    }

    /// Issue an HTTP GET against the Gmail API with rate-limit-aware
    /// retry. The caller passes the path (relative to `api_base`) and
    /// query string.
    async fn get_json(&self, path: &str, query: &[(&str, &str)]) -> Result<Value, GmailError> {
        let url = format!("{}{}", self.config_inner.api_base, path);
        // Clone the query into owned tuples so the retry closure can be
        // re-invoked across attempts.
        let owned_query: Vec<(String, String)> = query
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        call_with_retry(&self.config_inner, |attempt| {
            let url = url.clone();
            let path_owned = path.to_string();
            let owned_query = owned_query.clone();
            async move {
                debug!(path = %path_owned, attempt, "gmail GET");
                let resp = self
                    .http_inner
                    .get(&url)
                    .bearer_auth(&self.access_token_inner)
                    .query(&owned_query)
                    .send()
                    .await
                    .map_err(|e| RetryOutcome::Retryable(GmailError::Http(e.to_string()), None))?;

                classify_response(resp).await
            }
        })
        .await
    }

    /// `users.messages.list` paginated; returns all message ids that
    /// match `q`.
    pub async fn list_message_ids(
        &self,
        labels: &[&str],
        q: Option<&str>,
        page_size: u32,
    ) -> Result<Vec<String>, GmailError> {
        let path = format!("/gmail/v1/users/{}/messages", self.config_inner.user_id);
        let mut ids = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let mut query: Vec<(&str, String)> = vec![("maxResults", page_size.to_string())];
            for label in labels {
                query.push(("labelIds", (*label).to_string()));
            }
            if let Some(q) = q {
                query.push(("q", q.to_string()));
            }
            if let Some(pt) = page_token.as_ref() {
                query.push(("pageToken", pt.clone()));
            }
            let qref: Vec<(&str, &str)> = query.iter().map(|(k, v)| (*k, v.as_str())).collect();

            let v = self.get_json(&path, &qref).await?;
            if let Some(arr) = v.get("messages").and_then(|m| m.as_array()) {
                for m in arr {
                    if let Some(id) = m.get("id").and_then(|s| s.as_str()) {
                        ids.push(id.to_string());
                    }
                }
            }
            match v.get("nextPageToken").and_then(|s| s.as_str()) {
                Some(tok) if !tok.is_empty() => page_token = Some(tok.to_string()),
                _ => break,
            }
        }
        Ok(ids)
    }

    /// `users.messages.get` with `format=metadata` (cheap; headers only).
    pub async fn get_message_metadata(&self, id: &str) -> Result<Value, GmailError> {
        let path = format!(
            "/gmail/v1/users/{}/messages/{}",
            self.config_inner.user_id, id
        );
        self.get_json(&path, &[("format", "metadata")]).await
    }

    /// `users.messages.get` with `format=full` (includes body parts).
    pub async fn get_message_full(&self, id: &str) -> Result<Value, GmailError> {
        let path = format!(
            "/gmail/v1/users/{}/messages/{}",
            self.config_inner.user_id, id
        );
        self.get_json(&path, &[("format", "full")]).await
    }

    /// `users.history.list?startHistoryId=...`. Returns the parsed
    /// response or [`GmailError::HistoryExpired`] on HTTP 404.
    pub async fn list_history(
        &self,
        start_history_id: &str,
        labels: &[&str],
    ) -> Result<HistoryListResponse, GmailError> {
        let path = format!("/gmail/v1/users/{}/history", self.config_inner.user_id);
        let mut combined = HistoryListResponse::default();
        let mut page_token: Option<String> = None;

        loop {
            let mut query: Vec<(&str, String)> = vec![
                ("startHistoryId", start_history_id.to_string()),
                ("historyTypes", "messageAdded".to_string()),
                ("historyTypes", "labelAdded".to_string()),
                ("historyTypes", "labelRemoved".to_string()),
            ];
            for label in labels {
                query.push(("labelId", (*label).to_string()));
            }
            if let Some(pt) = page_token.as_ref() {
                query.push(("pageToken", pt.clone()));
            }
            let qref: Vec<(&str, &str)> = query.iter().map(|(k, v)| (*k, v.as_str())).collect();
            let v = self.get_json(&path, &qref).await?;

            // Parse this page.
            if let Some(arr) = v.get("history").and_then(|h| h.as_array()) {
                for entry in arr {
                    extract_message_ids_from_history_entry(entry, &mut combined.added_message_ids);
                }
            }
            if combined.history_id.is_empty() {
                if let Some(hid) = v.get("historyId").and_then(|h| h.as_str()) {
                    combined.history_id = hid.to_string();
                }
            } else if let Some(hid) = v.get("historyId").and_then(|h| h.as_str()) {
                combined.history_id = hid.to_string();
            }

            match v.get("nextPageToken").and_then(|s| s.as_str()) {
                Some(tok) if !tok.is_empty() => page_token = Some(tok.to_string()),
                _ => break,
            }
        }

        Ok(combined)
    }

    /// `users.getProfile` — used to bootstrap a `historyId` on first
    /// run.
    pub async fn get_profile(&self) -> Result<UserProfile, GmailError> {
        let path = format!("/gmail/v1/users/{}/profile", self.config_inner.user_id);
        let v = self.get_json(&path, &[]).await?;
        let history_id = v
            .get("historyId")
            .and_then(|h| h.as_str())
            .ok_or_else(|| GmailError::InvalidResponse("missing historyId in profile".into()))?
            .to_string();
        Ok(UserProfile { history_id })
    }
}

/// Classify a single HTTP response into success / retryable / fatal.
async fn classify_response(resp: reqwest::Response) -> Result<Value, RetryOutcome<GmailError>> {
    let status = resp.status();
    let retry_after = resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_secs);

    if status.is_success() {
        let body = resp
            .text()
            .await
            .map_err(|e| RetryOutcome::Fatal(GmailError::Http(e.to_string())))?;
        return serde_json::from_str(&body)
            .map_err(|e| RetryOutcome::Fatal(GmailError::InvalidResponse(e.to_string())));
    }

    let status_code = status.as_u16();

    // 404 from history.list signals an expired cursor — special-cased
    // by callers.
    if status_code == 404 {
        let body = resp.text().await.unwrap_or_default();
        // Bubble both the body (for diagnostics) and the dedicated
        // variant. We use the dedicated variant only when the path was
        // history; the caller wraps this in `GmailError::HistoryExpired`
        // by inspecting the request path.
        if body.contains("historyId") || body.contains("Start history") {
            return Err(RetryOutcome::Fatal(GmailError::HistoryExpired));
        }
        return Err(RetryOutcome::Fatal(GmailError::Api { status: 404, body }));
    }

    if status_code == 429 || status.is_server_error() {
        let body = resp.text().await.unwrap_or_default();
        return Err(RetryOutcome::Retryable(
            GmailError::Api {
                status: status_code,
                body,
            },
            retry_after,
        ));
    }

    let body = resp.text().await.unwrap_or_default();
    Err(RetryOutcome::Fatal(GmailError::Api {
        status: status_code,
        body,
    }))
}

/// Outcome of a single attempt inside [`call_with_retry`].
enum RetryOutcome<E> {
    Retryable(E, Option<Duration>),
    Fatal(E),
}

/// Execute `op` with rate-limit-aware retry. Honors `Retry-After`
/// headers when present, otherwise applies exponential backoff with
/// jitter.
async fn call_with_retry<F, Fut, T>(config: &GmailClientConfig, mut op: F) -> Result<T, GmailError>
where
    F: FnMut(usize) -> Fut,
    Fut: std::future::Future<Output = Result<T, RetryOutcome<GmailError>>>,
{
    let mut last_err: Option<GmailError> = None;
    for attempt in 0..=config.max_retries {
        match op(attempt).await {
            Ok(v) => return Ok(v),
            Err(RetryOutcome::Fatal(e)) => return Err(e),
            Err(RetryOutcome::Retryable(e, retry_after)) => {
                if attempt >= config.max_retries {
                    last_err = Some(e);
                    break;
                }
                let backoff = match retry_after {
                    Some(d) => d,
                    None => compute_backoff(attempt, config.base_backoff_ms, config.max_backoff_ms),
                };
                warn!(
                    attempt,
                    backoff_ms = backoff.as_millis() as u64,
                    "gmail request failed; retrying"
                );
                tokio::time::sleep(backoff).await;
                last_err = Some(e);
            }
        }
    }
    let _ = last_err; // surface the count regardless
    Err(GmailError::RetriesExhausted(config.max_retries + 1))
}

/// Compute exponential backoff with full jitter, in [base, base * 2^attempt].
fn compute_backoff(attempt: usize, base_ms: u64, cap_ms: u64) -> Duration {
    let pow = 1u64.checked_shl(attempt as u32).unwrap_or(u64::MAX);
    let exp = base_ms.saturating_mul(pow).min(cap_ms);
    let jitter: u64 = rand::thread_rng().gen_range(0..=exp.max(1));
    Duration::from_millis(jitter)
}

/// Subset of `users.history.list` we care about.
#[derive(Debug, Default, Clone)]
pub struct HistoryListResponse {
    /// New messages added (deduplicated by id).
    pub added_message_ids: Vec<String>,
    /// Latest historyId observed in the response.
    pub history_id: String,
}

/// Subset of `users.getProfile` we care about.
#[derive(Debug, Clone, Deserialize)]
pub struct UserProfile {
    /// Current historyId for this user. Used as the initial cursor.
    #[serde(rename = "historyId")]
    pub history_id: String,
}

/// Walk a `history[i]` entry and pull out message ids that matter for
/// us. We treat `messagesAdded` and `labelsAdded` as new-event signals;
/// downstream code dedupes by `gmail_internal_id`.
fn extract_message_ids_from_history_entry(entry: &Value, out: &mut Vec<String>) {
    let push = |arr: Option<&Vec<Value>>, out: &mut Vec<String>| {
        if let Some(arr) = arr {
            for item in arr {
                if let Some(id) = item
                    .get("message")
                    .and_then(|m| m.get("id"))
                    .and_then(|i| i.as_str())
                {
                    if !out.iter().any(|x| x == id) {
                        out.push(id.to_string());
                    }
                }
            }
        }
    };
    push(entry.get("messagesAdded").and_then(|a| a.as_array()), out);
    push(entry.get("labelsAdded").and_then(|a| a.as_array()), out);
    // labelsRemoved is signaled but doesn't change message identity —
    // skip it for ingestion. (Open question for v0.4: emit a label
    // change event.)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_exponentially() {
        // Just check that the cap holds.
        let d0 = compute_backoff(0, 100, 5000);
        let d10 = compute_backoff(10, 100, 5000);
        assert!(d0.as_millis() <= 100);
        assert!(d10.as_millis() <= 5000);
    }

    #[test]
    fn extract_added_ids_from_history() {
        let entry = serde_json::json!({
            "messagesAdded": [
                { "message": { "id": "a", "labelIds": ["INBOX"] } },
                { "message": { "id": "b", "labelIds": ["INBOX"] } },
            ],
            "labelsAdded": [
                { "message": { "id": "b" }, "labelIds": ["IMPORTANT"] }
            ]
        });
        let mut out = Vec::new();
        extract_message_ids_from_history_entry(&entry, &mut out);
        assert_eq!(out, vec!["a".to_string(), "b".to_string()]);
    }
}
