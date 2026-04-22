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
    owner: String,
    /// The repository name to watch, or None to watch all repos for the owner.
    repo: Option<String>,
}

impl GitHubAdapter {
    /// Create a new GitHub adapter.
    ///
    /// # Arguments
    /// * `owner` - The GitHub owner (user or organization)
    /// * `repo` - Optional repository name (None to watch all repos)
    pub fn new(owner: String, repo: Option<String>) -> Self {
        Self { owner, repo }
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
        todo!(
            "GitHub adapter not yet implemented — requires API token and GitHub API integration for {}/{}",
            self.owner,
            self.repo.as_deref().unwrap_or("*")
        )
    }
}
