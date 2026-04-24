//! [`Adapter`] trait implementation.

use std::sync::Arc;

use ctxd_adapter_core::{Adapter, AdapterError, EventSink};
use tracing::info;

use crate::config::Config;
use crate::poller::Poller;
use crate::state::StateDb;

/// GitHub adapter that ingests issues, PRs, comments, and notifications.
pub struct GitHubAdapter {
    config: Config,
}

impl GitHubAdapter {
    /// Build a new adapter from a resolved [`Config`].
    pub fn new(config: Config) -> Self {
        Self { config }
    }

    /// Open the state DB at the configured location.
    pub async fn open_state(&self) -> Result<Arc<StateDb>, AdapterError> {
        let state = StateDb::open(&self.config.state_dir)
            .await
            .map_err(|e| AdapterError::Internal(format!("opening state dir: {e}")))?;
        Ok(Arc::new(state))
    }

    /// Run a single poll cycle (used in tests + the `run --once` path).
    pub async fn run_once(&self, sink: &dyn EventSink) -> Result<(), AdapterError> {
        let state = self.open_state().await?;
        let poller = Poller::new(self.config.clone(), state)
            .map_err(|e| AdapterError::Internal(format!("client init: {e}")))?;
        poller.poll_once(sink).await
    }
}

#[async_trait::async_trait]
impl Adapter for GitHubAdapter {
    fn name(&self) -> &str {
        "github"
    }

    fn subject_prefix(&self) -> &str {
        "/work/github"
    }

    async fn run(&self, sink: Box<dyn EventSink>) -> Result<(), AdapterError> {
        info!(
            api_base = %self.config.api_base,
            poll_interval_ms = self.config.poll_interval.as_millis() as u64,
            "starting github adapter"
        );
        let state = self.open_state().await?;
        let poller = Poller::new(self.config.clone(), state)
            .map_err(|e| AdapterError::Internal(format!("client init: {e}")))?;
        poller.run(sink).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(api: &str) -> Config {
        Config {
            api_base: api.to_string(),
            token: "tk".to_string(),
            repos: crate::config::RepoSelector::Explicit(vec![]),
            state_dir: tempfile::tempdir().unwrap().keep(),
            poll_interval: std::time::Duration::from_millis(50),
            kinds: vec![],
            include_notifications: false,
            max_cycles: Some(1),
        }
    }

    #[tokio::test]
    async fn name_and_prefix() {
        let a = GitHubAdapter::new(cfg("https://api.github.com"));
        assert_eq!(a.name(), "github");
        assert_eq!(a.subject_prefix(), "/work/github");
    }
}
