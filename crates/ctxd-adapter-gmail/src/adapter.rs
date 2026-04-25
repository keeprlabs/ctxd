//! [`GmailAdapter`] — the top-level glue that ties together the
//! [`oauth`](crate::oauth), [`crypto`](crate::crypto),
//! [`gmail`](crate::gmail), [`parse`](crate::parse), and
//! [`state`](crate::state) modules and implements the
//! [`ctxd_adapter_core::Adapter`] trait.
//!
//! # Sync loop
//!
//! On each tick of the poll interval:
//!
//! 1. If we have no cursor, run a full sync via `users.messages.list`,
//!    record the current `historyId` from `users.getProfile`.
//! 2. Otherwise, call `users.history.list?startHistoryId=<cursor>`.
//!    On [`gmail::GmailError::HistoryExpired`] (HTTP 404), fall back
//!    to a full sync.
//! 3. For each new message id (deduplicated via [`state::StateStore`]):
//!     - Fetch metadata + full body in parallel (concurrency =
//!       [`crate::DEFAULT_FETCH_CONCURRENCY`]).
//!     - Build one event per label and publish via the
//!       [`ctxd_adapter_core::EventSink`].
//!     - Record `(gmail_internal_id, label)` as published.
//! 4. Update the cursor.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ctxd_adapter_core::{Adapter, AdapterError, EventSink};
use serde_json::Value;
use tokio::sync::Mutex;
use tokio::task::JoinSet;
use tracing::{debug, info, warn};

use crate::crypto;
use crate::gmail::{GmailClient, GmailClientConfig, GmailError};
use crate::oauth::{self, AuthorizedTokens, OAuthConfig};
use crate::parse;
use crate::state::StateStore;

/// Configuration for the runtime adapter.
#[derive(Debug, Clone)]
pub struct GmailAdapterConfig {
    /// State directory holding the encrypted token, master key, and
    /// SQLite state.
    pub state_dir: PathBuf,
    /// Gmail user ID (typically `me`).
    pub user_id: String,
    /// Labels to sync, e.g. `["INBOX", "SENT"]`.
    pub labels: Vec<String>,
    /// Polling interval between history syncs.
    pub poll_interval: Duration,
    /// OAuth config used for refresh.
    pub oauth: OAuthConfig,
    /// Gmail client base config (api_base, retries).
    pub gmail: GmailClientConfig,
    /// Run only one sync iteration and then stop (useful for tests).
    pub run_once: bool,
    /// Override for the encrypted token path (defaults to
    /// `<state-dir>/gmail.token.enc`).
    pub token_path_override: Option<PathBuf>,
    /// Override for the master key path (defaults to
    /// `<state-dir>/gmail.key`).
    pub key_path_override: Option<PathBuf>,
    /// Override for the state-DB path (defaults to
    /// `<state-dir>/gmail.state.db`).
    pub db_path_override: Option<PathBuf>,
}

impl GmailAdapterConfig {
    /// Path to the encrypted refresh-token file.
    pub fn token_path(&self) -> PathBuf {
        self.token_path_override
            .clone()
            .unwrap_or_else(|| self.state_dir.join("gmail.token.enc"))
    }

    /// Path to the master-key file.
    pub fn key_path(&self) -> PathBuf {
        self.key_path_override
            .clone()
            .unwrap_or_else(|| self.state_dir.join("gmail.key"))
    }

    /// Path to the state DB.
    pub fn db_path(&self) -> PathBuf {
        self.db_path_override
            .clone()
            .unwrap_or_else(|| self.state_dir.join("gmail.state.db"))
    }
}

/// Gmail adapter — runs the sync loop and publishes events into ctxd.
pub struct GmailAdapter {
    config: GmailAdapterConfig,
}

impl GmailAdapter {
    /// Create a new adapter from the given configuration.
    pub fn new(config: GmailAdapterConfig) -> Self {
        Self { config }
    }

    /// Read the encrypted token from disk and decrypt it.
    async fn load_refresh_token(&self) -> Result<String, AdapterError> {
        let key = crypto::load_master_key(&self.config.key_path())
            .await
            .map_err(|e| AdapterError::Internal(format!("loading master key: {e}")))?;
        let blob = tokio::fs::read(&self.config.token_path())
            .await
            .map_err(|e| AdapterError::Internal(format!("loading token: {e}")))?;
        let plaintext = crypto::decrypt(&key, &blob)
            .map_err(|e| AdapterError::Internal(format!("decrypting token: {e}")))?;
        String::from_utf8(plaintext)
            .map_err(|_| AdapterError::Internal("decrypted token is not utf-8".into()))
    }

    /// Run a single iteration of the sync loop.
    async fn sync_once(
        &self,
        client: &mut GmailClient,
        store: &StateStore,
        sink: &dyn EventSink,
    ) -> Result<(), AdapterError> {
        let cursor = store
            .cursor()
            .await
            .map_err(|e| AdapterError::Internal(format!("reading cursor: {e}")))?;

        let label_refs: Vec<&str> = self.config.labels.iter().map(|s| s.as_str()).collect();

        let (message_ids, new_history_id) = match cursor.history_id.as_deref() {
            None => {
                info!("no cursor; running initial full sync");
                self.full_sync(client, &label_refs).await?
            }
            Some(hid) => match client.list_history(hid, &label_refs).await {
                Ok(resp) => {
                    debug!(
                        added = resp.added_message_ids.len(),
                        history_id = %resp.history_id,
                        "history.list ok"
                    );
                    let new_hid = if resp.history_id.is_empty() {
                        hid.to_string()
                    } else {
                        resp.history_id
                    };
                    (resp.added_message_ids, new_hid)
                }
                Err(GmailError::HistoryExpired) => {
                    warn!("history cursor expired; falling back to full sync");
                    self.full_sync(client, &label_refs).await?
                }
                Err(e) => return Err(map_gmail_error(e)),
            },
        };

        if !message_ids.is_empty() {
            self.fetch_and_publish(client, store, sink, message_ids)
                .await?;
        }

        store
            .set_cursor(&new_history_id, chrono::Utc::now())
            .await
            .map_err(|e| AdapterError::Internal(format!("writing cursor: {e}")))?;
        Ok(())
    }

    /// Full sync via `messages.list` + `getProfile`. Returns the list
    /// of new message ids and the historyId to record as the new cursor.
    async fn full_sync(
        &self,
        client: &GmailClient,
        labels: &[&str],
    ) -> Result<(Vec<String>, String), AdapterError> {
        let profile = client.get_profile().await.map_err(map_gmail_error)?;
        let ids = client
            .list_message_ids(labels, None, 100)
            .await
            .map_err(map_gmail_error)?;
        Ok((ids, profile.history_id))
    }

    /// Fetch each message in parallel (bounded concurrency) and publish
    /// one event per label.
    async fn fetch_and_publish(
        &self,
        client: &GmailClient,
        store: &StateStore,
        sink: &dyn EventSink,
        ids: Vec<String>,
    ) -> Result<(), AdapterError> {
        let concurrency = crate::DEFAULT_FETCH_CONCURRENCY;
        let client = Arc::new(client.clone_handle());

        // We can't easily use rayon-style parallelism for fetch+publish
        // because the EventSink is `&dyn` and we need owned Arc-style
        // sharing. Instead, fetch in parallel, publish sequentially.
        let fetched = Arc::new(Mutex::new(Vec::with_capacity(ids.len())));
        let mut set: JoinSet<Result<(), AdapterError>> = JoinSet::new();

        // Cap parallelism with a semaphore.
        let permits = Arc::new(tokio::sync::Semaphore::new(concurrency));

        for id in ids {
            let client = client.clone();
            let fetched = fetched.clone();
            let permits = permits.clone();
            set.spawn(async move {
                let _permit = permits
                    .acquire_owned()
                    .await
                    .map_err(|e| AdapterError::Internal(format!("semaphore: {e}")))?;
                match client.get_message_full(&id).await {
                    Ok(v) => {
                        fetched.lock().await.push((id, v));
                        Ok(())
                    }
                    Err(e) => {
                        warn!(id, err = %e, "failed to fetch message; skipping");
                        Ok(())
                    }
                }
            });
        }

        while let Some(joined) = set.join_next().await {
            joined.map_err(|e| AdapterError::Internal(format!("join: {e}")))??;
        }

        let mut fetched = Arc::try_unwrap(fetched)
            .map_err(|_| AdapterError::Internal("fetched arc still has holders".into()))?
            .into_inner();
        // Stable order — sort by id so test runs are deterministic.
        fetched.sort_by(|a, b| a.0.cmp(&b.0));

        for (id, message) in fetched {
            if let Err(e) = self.publish_message(store, sink, &id, &message).await {
                warn!(id, err = %e, "failed to publish message; continuing");
            }
        }
        Ok(())
    }

    /// Build event(s) for a single Gmail message and dispatch them via
    /// the sink. One event per non-internal label.
    async fn publish_message(
        &self,
        store: &StateStore,
        sink: &dyn EventSink,
        id: &str,
        message: &Value,
    ) -> Result<(), AdapterError> {
        let internal_id = message
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or(id)
            .to_string();
        let thread_id = message
            .get("threadId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let snippet = message
            .get("snippet")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let labels: Vec<String> = message
            .get("labelIds")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let payload = match message.get("payload") {
            Some(p) => p,
            None => {
                warn!(id, "message missing payload; skipping");
                return Ok(());
            }
        };

        let headers_arr: Vec<Value> = payload
            .get("headers")
            .and_then(|h| h.as_array())
            .cloned()
            .unwrap_or_default();

        let from = parse::extract_header(&headers_arr, "From").unwrap_or_default();
        let to = parse::extract_header(&headers_arr, "To").unwrap_or_default();
        let cc = parse::extract_header(&headers_arr, "Cc").unwrap_or_default();
        let bcc = parse::extract_header(&headers_arr, "Bcc").unwrap_or_default();
        let subject_hdr = parse::extract_header(&headers_arr, "Subject").unwrap_or_default();
        let date_hdr = parse::extract_header(&headers_arr, "Date").unwrap_or_default();
        let message_id_hdr = parse::extract_header(&headers_arr, "Message-ID").unwrap_or_default();
        let list_id = parse::extract_header(&headers_arr, "List-Id");

        let body = parse::extract_body(payload);

        let date_rfc3339 = chrono::DateTime::parse_from_rfc2822(&date_hdr)
            .map(|d| d.with_timezone(&chrono::Utc).to_rfc3339())
            .unwrap_or_else(|_| date_hdr.clone());

        let event_type = parse::infer_event_type(&labels);

        let data = serde_json::json!({
            "from": from,
            "to": parse::split_addresses(&to),
            "cc": parse::split_addresses(&cc),
            "bcc": parse::split_addresses(&bcc),
            "subject": subject_hdr,
            "snippet": snippet,
            "body": body,
            "date_rfc3339": date_rfc3339,
            "message_id": message_id_hdr,
            "thread_id": thread_id,
            "labels": labels,
            "list_id": list_id,
            "gmail_internal_id": internal_id,
        });

        // One event per label. If the message has no labels (rare),
        // publish under `_` so we don't drop the event.
        let labels_for_subject: Vec<String> = if labels.is_empty() {
            vec!["_".to_string()]
        } else {
            labels.clone()
        };

        for label in &labels_for_subject {
            let normalized = parse::normalize_label(label);
            let already = store
                .is_published(&internal_id, &normalized)
                .await
                .map_err(|e| AdapterError::Internal(format!("idempotency check: {e}")))?;
            if already {
                debug!(internal_id, label = %normalized, "already published; skipping");
                continue;
            }
            let subject = parse::subject_for_message(label, &internal_id);
            match sink.publish(&subject, event_type, data.clone()).await {
                Ok(_id) => {
                    store
                        .mark_published(&internal_id, &normalized)
                        .await
                        .map_err(|e| AdapterError::Internal(format!("recording publish: {e}")))?;
                    debug!(subject, event_type, "published gmail event");
                }
                Err(e) => {
                    warn!(subject, err = %e, "sink rejected event");
                }
            }
        }

        Ok(())
    }
}

/// Map a [`GmailError`] to an [`AdapterError`].
fn map_gmail_error(e: GmailError) -> AdapterError {
    AdapterError::Internal(format!("gmail: {e}"))
}

#[async_trait::async_trait]
impl Adapter for GmailAdapter {
    fn name(&self) -> &str {
        "gmail"
    }

    fn subject_prefix(&self) -> &str {
        "/work/email/gmail"
    }

    async fn run(&self, sink: Box<dyn EventSink>) -> Result<(), AdapterError> {
        info!(
            user = %self.config.user_id,
            labels = ?self.config.labels,
            "starting gmail adapter"
        );

        // Load + decrypt the refresh token.
        let refresh_token = self.load_refresh_token().await?;

        // Build a single connection-pooled HTTP client.
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| AdapterError::Internal(format!("http client: {e}")))?;

        // First-token: refresh.
        let tokens = oauth::refresh_access_token(&http, &self.config.oauth, &refresh_token)
            .await
            .map_err(|e| AdapterError::Internal(format!("oauth refresh: {e}")))?;

        // If Google rotated the refresh token (rare for Google but
        // allowed by RFC), re-encrypt + persist.
        if tokens.refresh_token != refresh_token {
            let key = crypto::load_master_key(&self.config.key_path())
                .await
                .map_err(|e| AdapterError::Internal(format!("loading master key: {e}")))?;
            let blob = crypto::encrypt(&key, tokens.refresh_token.as_bytes())
                .map_err(|e| AdapterError::Internal(format!("encrypting token: {e}")))?;
            crypto::write_secret_file(&self.config.token_path(), &blob)
                .await
                .map_err(|e| AdapterError::Internal(format!("writing token: {e}")))?;
        }

        let mut current_tokens = tokens;
        let mut client = GmailClient::new(
            http.clone(),
            self.config.gmail.clone(),
            current_tokens.access_token.clone(),
        );

        let store = StateStore::open(&self.config.db_path())
            .await
            .map_err(|e| AdapterError::Internal(format!("opening state db: {e}")))?;

        loop {
            // Refresh access token if it's about to expire.
            if access_token_expired_soon(&current_tokens) {
                debug!("access token expiring soon; refreshing");
                match oauth::refresh_access_token(&http, &self.config.oauth, &refresh_token).await {
                    Ok(t) => {
                        current_tokens = t;
                        client.set_access_token(current_tokens.access_token.clone());
                    }
                    Err(e) => {
                        warn!(err = %e, "refresh failed; continuing with current token");
                    }
                }
            }

            if let Err(e) = self.sync_once(&mut client, &store, sink.as_ref()).await {
                warn!(err = %e, "sync iteration failed");
            }

            if self.config.run_once {
                break;
            }

            tokio::time::sleep(self.config.poll_interval).await;
        }

        Ok(())
    }
}

/// Returns true when the access token expires in under 60 seconds.
fn access_token_expired_soon(t: &AuthorizedTokens) -> bool {
    let now = chrono::Utc::now();
    (t.expires_at - now) < chrono::Duration::seconds(60)
}

// ===== GmailClient::clone_handle ===========================================

impl GmailClient {
    /// Clone the client into an `Arc`-shareable handle. Uses `Clone` on
    /// the underlying `reqwest::Client` (which is cheap) and dupes the
    /// access token.
    pub fn clone_handle(&self) -> GmailClient {
        GmailClient::new(
            self.http().clone(),
            self.config().clone(),
            self.access_token().to_string(),
        )
    }

    /// Borrow the inner HTTP client.
    pub fn http(&self) -> &reqwest::Client {
        &self.http_inner
    }

    /// Borrow the client config.
    pub fn config(&self) -> &GmailClientConfig {
        &self.config_inner
    }

    /// Borrow the current access token. Never log this.
    pub fn access_token(&self) -> &str {
        &self.access_token_inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_path_default() {
        let cfg = make_config();
        let p = cfg.token_path();
        assert!(p.ends_with("gmail.token.enc"));
    }

    #[test]
    fn key_path_default() {
        let cfg = make_config();
        let p = cfg.key_path();
        assert!(p.ends_with("gmail.key"));
    }

    fn make_config() -> GmailAdapterConfig {
        GmailAdapterConfig {
            state_dir: PathBuf::from("/tmp/x"),
            user_id: "me".to_string(),
            labels: vec!["INBOX".to_string()],
            poll_interval: Duration::from_secs(60),
            oauth: OAuthConfig::google(
                "id".to_string(),
                "secret".to_string(),
                crate::GMAIL_SCOPE.to_string(),
            ),
            gmail: GmailClientConfig::default(),
            run_once: false,
            token_path_override: None,
            key_path_override: None,
            db_path_override: None,
        }
    }
}
