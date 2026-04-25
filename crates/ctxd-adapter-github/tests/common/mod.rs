//! Shared test helpers for the github adapter integration tests.
#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use ctxd_adapter_core::{AdapterError, EventSink};
use ctxd_adapter_github::config::{Config, RepoRef, RepoSelector, ResourceKind};
use serde_json::Value;
use tokio::sync::Mutex;

/// One captured event.
#[derive(Debug, Clone)]
pub struct Captured {
    pub subject: String,
    pub event_type: String,
    pub data: Value,
}

/// A sink that records every event in-memory.
pub struct CollectingSink {
    pub events: Arc<Mutex<Vec<Captured>>>,
}

impl CollectingSink {
    pub fn new() -> (Self, Arc<Mutex<Vec<Captured>>>) {
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
        self.events.lock().await.push(Captured {
            subject: subject.to_string(),
            event_type: event_type.to_string(),
            data,
        });
        Ok(uuid::Uuid::now_v7().to_string())
    }
}

/// Build a config that runs exactly one cycle against the given mock URI.
pub fn explicit_config(
    mock_uri: &str,
    state_dir: std::path::PathBuf,
    repos: Vec<RepoRef>,
    kinds: Vec<ResourceKind>,
    include_notifications: bool,
) -> Config {
    Config {
        api_base: mock_uri.to_string(),
        token: "test-token".to_string(),
        repos: RepoSelector::Explicit(repos),
        state_dir,
        poll_interval: Duration::from_millis(10),
        kinds,
        include_notifications,
        max_cycles: Some(1),
    }
}
