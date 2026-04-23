//! GitHub adapter for ctxd.
//!
//! When fully implemented, this adapter will:
//! - Connect to the GitHub API using a personal access token or GitHub App credentials
//! - Poll for new issues, pull requests, comments, and notifications
//! - Publish events under `/work/github/{owner}/{repo}/{type}/{number}`
//! - Event types: `issue.opened`, `issue.closed`, `pr.opened`, `pr.merged`,
//!   `comment.created`, `notification.received`
//! - Event data includes: title, body, author, labels, state, timestamps
//! - Support webhook-based real-time ingestion as an alternative to polling
//! - Handle pagination and rate limiting

use ctxd_adapter_core::{Adapter, AdapterError, EventSink};

/// GitHub adapter that ingests issues, PRs, and notifications via the GitHub API.
pub struct GitHubAdapter {
    /// The GitHub owner (user or org) to watch.
    _owner: String,
    /// The repository name to watch, or None to watch all repos for the owner.
    _repo: Option<String>,
}

impl GitHubAdapter {
    /// Create a new GitHub adapter.
    ///
    /// # Arguments
    /// * `owner` - The GitHub owner (user or organization)
    /// * `repo` - Optional repository name (None to watch all repos)
    pub fn new(owner: String, repo: Option<String>) -> Self {
        Self {
            _owner: owner,
            _repo: repo,
        }
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

    async fn run(&self, _sink: Box<dyn EventSink>) -> Result<(), AdapterError> {
        Err(AdapterError::Internal(
            "GitHub adapter not yet implemented. See docs/adapter-guide.md".to_string(),
        ))
    }
}
