//! Shared helpers for the adapter integration tests.
//!
//! Each helper is `#[allow(dead_code)]` because rustc rebuilds this
//! module per integration test and not every test calls every helper.

#![allow(dead_code)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use ctxd_adapter_core::{AdapterError, EventSink};
use ctxd_adapter_gmail::{
    crypto, gmail::GmailClientConfig, oauth::OAuthConfig, GmailAdapter, GmailAdapterConfig,
    GMAIL_SCOPE,
};
use serde_json::Value;
use tokio::sync::Mutex;

/// Collected event: (subject, event_type, data).
pub type CollectedEvent = (String, String, Value);

/// Prepare a fresh state-dir with an encrypted refresh token whose
/// plaintext is `refresh_token`. Returns the directory handle (must be
/// kept alive for the duration of the test) and the path itself.
pub async fn fresh_state_dir(refresh_token: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tmpdir");
    let path = dir.path().to_path_buf();
    let key_path = path.join("gmail.key");
    let token_path = path.join("gmail.token.enc");

    let key = crypto::load_or_create_master_key(&key_path)
        .await
        .expect("master key");
    let blob = crypto::encrypt(&key, refresh_token.as_bytes()).expect("encrypt");
    crypto::write_secret_file(&token_path, &blob)
        .await
        .expect("write token");

    (dir, path)
}

/// Build an adapter configured to talk to the given mock URLs and run
/// for exactly one sync iteration.
pub fn make_adapter_config(
    state_dir: &Path,
    token_url: &str,
    api_base: &str,
    labels: Vec<String>,
) -> GmailAdapterConfig {
    let oauth = OAuthConfig {
        client_id: "test-client".into(),
        client_secret: "test-secret".into(),
        scope: GMAIL_SCOPE.into(),
        device_code_url: format!("{token_url}/device/code"),
        token_url: token_url.to_string(),
    };
    let gmail_cfg = GmailClientConfig {
        api_base: api_base.to_string(),
        user_id: "me".into(),
        max_retries: 3,
        base_backoff_ms: 50,
        max_backoff_ms: 500,
    };
    GmailAdapterConfig {
        state_dir: state_dir.to_path_buf(),
        user_id: "me".into(),
        labels,
        poll_interval: Duration::from_millis(100),
        oauth,
        gmail: gmail_cfg,
        run_once: true,
        token_path_override: None,
        key_path_override: None,
        db_path_override: None,
    }
}

/// Test sink that collects published events.
pub struct CollectingSink {
    pub events: Arc<Mutex<Vec<CollectedEvent>>>,
}

impl CollectingSink {
    pub fn new() -> (Self, Arc<Mutex<Vec<CollectedEvent>>>) {
        let events = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                events: events.clone(),
            },
            events,
        )
    }
}

#[async_trait::async_trait]
impl EventSink for CollectingSink {
    async fn publish(
        &self,
        subject: &str,
        event_type: &str,
        data: Value,
    ) -> Result<String, AdapterError> {
        let mut events = self.events.lock().await;
        let id = format!("evt-{}", events.len());
        events.push((subject.to_string(), event_type.to_string(), data));
        Ok(id)
    }
}

/// Run the adapter with a `CollectingSink` for one iteration.
pub async fn run_once(
    cfg: GmailAdapterConfig,
) -> (Result<(), AdapterError>, Arc<Mutex<Vec<CollectedEvent>>>) {
    use ctxd_adapter_core::Adapter;
    let (sink, events) = CollectingSink::new();
    let adapter = GmailAdapter::new(cfg);
    let result = adapter.run(Box::new(sink)).await;
    (result, events)
}

/// Encode raw bytes as Gmail-style url-safe base64 (no padding).
pub fn b64url(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Pull-out helper: count events with the given event_type.
pub async fn events_of_type(
    events: &Arc<Mutex<Vec<CollectedEvent>>>,
    event_type: &str,
) -> Vec<CollectedEvent> {
    let guard = events.lock().await;
    guard
        .iter()
        .filter(|(_, t, _)| t == event_type)
        .cloned()
        .collect()
}
