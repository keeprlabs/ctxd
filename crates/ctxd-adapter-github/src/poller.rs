//! Main polling loop.
//!
//! Each cycle: for every configured repo, fetch issues / PRs / comments
//! since the last cursor; for the user, fetch notifications. After each
//! resource page, we publish events for any item with
//! `updated_at > stored cursor` and bump the cursor and seen-resources
//! tables.

use std::sync::Arc;

use ctxd_adapter_core::{AdapterError, EventSink};
use serde_json::Value;
use tracing::{debug, info, warn};

use crate::client::{GhClient, HttpError};
use crate::config::{Config, RepoRef, RepoSelector, ResourceKind};
use crate::events::{
    classify_comment, classify_issue, classify_notification, classify_pr, comment_payload,
    issue_comment_subject, issue_number_from_url, issue_payload, issue_subject,
    notification_payload, notification_subject, pr_comment_subject, pr_number_from_url, pr_payload,
    pr_subject,
};
use crate::state::StateDb;

/// The poller orchestrates one or more polling cycles.
pub struct Poller {
    cfg: Config,
    state: Arc<StateDb>,
    client: GhClient,
}

impl Poller {
    /// Build a poller from config + state.
    pub fn new(cfg: Config, state: Arc<StateDb>) -> Result<Self, HttpError> {
        let client = GhClient::new(cfg.api_base.clone(), cfg.token.clone(), state.clone())?;
        Ok(Self { cfg, state, client })
    }

    /// Override the client (used by tests with custom retry settings).
    pub fn with_client(mut self, client: GhClient) -> Self {
        self.client = client;
        self
    }

    /// Run a single polling cycle.
    pub async fn poll_once(&self, sink: &dyn EventSink) -> Result<(), AdapterError> {
        let repos = self.resolve_repos().await.map_err(adapt_err)?;
        for r in &repos {
            for kind in &self.cfg.kinds {
                match kind {
                    ResourceKind::Issues => {
                        if let Err(e) = self.poll_issues(r, sink).await {
                            warn!(repo = %r.slug(), error = %e, "issues poll failed");
                        }
                    }
                    ResourceKind::Pulls => {
                        if let Err(e) = self.poll_pulls(r, sink).await {
                            warn!(repo = %r.slug(), error = %e, "pulls poll failed");
                        }
                    }
                    ResourceKind::Comments => {
                        if let Err(e) = self.poll_issue_comments(r, sink).await {
                            warn!(repo = %r.slug(), error = %e, "issue comments poll failed");
                        }
                        if let Err(e) = self.poll_pr_comments(r, sink).await {
                            warn!(repo = %r.slug(), error = %e, "pr comments poll failed");
                        }
                    }
                    ResourceKind::Notifications => {
                        // notifications handled outside the per-repo loop
                    }
                }
            }
        }

        if self.cfg.has_kind(ResourceKind::Notifications) && self.cfg.include_notifications {
            if let Err(e) = self.poll_notifications(sink).await {
                warn!(error = %e, "notifications poll failed");
            }
        }

        let now = chrono::Utc::now().to_rfc3339();
        if let Err(e) = self.state.put_meta("last_poll_at", &now).await {
            warn!(?e, "failed to record last_poll_at");
        }

        Ok(())
    }

    /// Run cycles indefinitely (or up to `max_cycles` if configured).
    pub async fn run(&self, sink: Box<dyn EventSink>) -> Result<(), AdapterError> {
        let mut cycle: u32 = 0;
        loop {
            cycle += 1;
            info!(cycle, "github polling cycle starting");
            self.poll_once(sink.as_ref()).await?;
            if let Some(max) = self.cfg.max_cycles {
                if cycle >= max {
                    info!(cycle, "reached max_cycles; stopping");
                    return Ok(());
                }
            }
            tokio::time::sleep(self.cfg.poll_interval).await;
        }
    }

    /// Resolve the configured repo list (either explicit or via `/user/repos`).
    async fn resolve_repos(&self) -> Result<Vec<RepoRef>, HttpError> {
        match &self.cfg.repos {
            RepoSelector::Explicit(list) => Ok(list.clone()),
            RepoSelector::AuthenticatedUser => {
                let mut out = Vec::new();
                self.client
                    .fetch_all("/user/repos?per_page=100&sort=updated", |body| {
                        let arr = body.as_array().cloned().unwrap_or_default();
                        for r in arr {
                            let owner = r
                                .get("owner")
                                .and_then(|o| o.get("login"))
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string();
                            let name = r
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string();
                            if !owner.is_empty() && !name.is_empty() {
                                out.push(RepoRef { owner, name });
                            }
                        }
                        Ok(())
                    })
                    .await?;
                Ok(out)
            }
        }
    }

    /// Build a `since=<cursor>` query suffix or empty string when no cursor exists.
    fn since_suffix(cursor: Option<&str>) -> String {
        match cursor {
            Some(c) => format!("&since={c}"),
            None => String::new(),
        }
    }

    async fn poll_issues(&self, r: &RepoRef, sink: &dyn EventSink) -> Result<(), HttpError> {
        let scope = r.slug();
        let cursor = self.state.get_cursor(&scope, "issues").await?;
        let path = format!(
            "/repos/{}/{}/issues?state=all&per_page=100&sort=updated&direction=asc{}",
            r.owner,
            r.name,
            Self::since_suffix(cursor.as_deref()),
        );
        let mut max_seen: Option<String> = cursor.clone();
        let owner = r.owner.clone();
        let name = r.name.clone();
        let state = self.state.clone();

        let mut buffered: Vec<Value> = Vec::new();
        self.client
            .fetch_all(&path, |body| {
                if let Some(arr) = body.as_array() {
                    buffered.extend(arr.iter().cloned());
                }
                Ok(())
            })
            .await?;

        for item in buffered {
            // The /issues endpoint includes PRs; skip them (handled by poll_pulls).
            if item.get("pull_request").is_some() {
                continue;
            }
            let updated_at = item
                .get("updated_at")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let number = item
                .get("number")
                .and_then(Value::as_i64)
                .unwrap_or_default();
            if number == 0 || updated_at.is_empty() {
                continue;
            }
            let resource_key = format!("{owner}/{name}/{number}");
            let prior = state.get_seen("issue", &resource_key).await?;
            // Idempotency: skip if updated_at <= prior.
            if let Some((prev, _)) = &prior {
                if &updated_at <= prev {
                    debug!(resource_key, "skip: updated_at not newer");
                    continue;
                }
            }
            let cur_state = item
                .get("state")
                .and_then(Value::as_str)
                .unwrap_or("open")
                .to_string();
            let class = classify_issue(prior.is_some(), &cur_state);
            let payload = issue_payload(&owner, &name, &item);
            let subject = issue_subject(&owner, &name, number);
            sink.publish(&subject, class.event_type, payload)
                .await
                .map_err(|e| HttpError::Status {
                    status: reqwest::StatusCode::INTERNAL_SERVER_ERROR,
                    body: format!("sink: {e}"),
                })?;
            state
                .put_seen("issue", &resource_key, &updated_at, Some(&cur_state))
                .await?;
            if max_seen.as_ref().is_none_or(|m| m < &updated_at) {
                max_seen = Some(updated_at);
            }
        }

        if let Some(m) = max_seen {
            self.state.put_cursor(&scope, "issues", &m).await?;
        }
        Ok(())
    }

    async fn poll_pulls(&self, r: &RepoRef, sink: &dyn EventSink) -> Result<(), HttpError> {
        let scope = r.slug();
        let cursor = self.state.get_cursor(&scope, "pulls").await?;
        // The /pulls endpoint does NOT support `since`; it does support
        // sort=updated&direction=asc. We still apply the cursor client-side
        // for idempotency.
        let path = format!(
            "/repos/{}/{}/pulls?state=all&per_page=100&sort=updated&direction=asc",
            r.owner, r.name,
        );
        let owner = r.owner.clone();
        let name = r.name.clone();
        let state = self.state.clone();

        let mut buffered: Vec<Value> = Vec::new();
        self.client
            .fetch_all(&path, |body| {
                if let Some(arr) = body.as_array() {
                    buffered.extend(arr.iter().cloned());
                }
                Ok(())
            })
            .await?;

        let mut max_seen: Option<String> = cursor.clone();
        for item in buffered {
            let updated_at = item
                .get("updated_at")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let number = item
                .get("number")
                .and_then(Value::as_i64)
                .unwrap_or_default();
            if number == 0 || updated_at.is_empty() {
                continue;
            }
            if let Some(c) = &cursor {
                if &updated_at <= c {
                    continue;
                }
            }
            let resource_key = format!("{owner}/{name}/{number}");
            let prior = state.get_seen("pr", &resource_key).await?;
            if let Some((prev, _)) = &prior {
                if &updated_at <= prev {
                    continue;
                }
            }
            let cur_state = item
                .get("state")
                .and_then(Value::as_str)
                .unwrap_or("open")
                .to_string();
            let merged = item.get("merged").and_then(Value::as_bool).unwrap_or(false);
            let class = classify_pr(prior.is_some(), &cur_state, merged);
            let payload = pr_payload(&owner, &name, &item);
            let subject = pr_subject(&owner, &name, number);
            sink.publish(&subject, class.event_type, payload)
                .await
                .map_err(|e| HttpError::Status {
                    status: reqwest::StatusCode::INTERNAL_SERVER_ERROR,
                    body: format!("sink: {e}"),
                })?;
            state
                .put_seen("pr", &resource_key, &updated_at, Some(&cur_state))
                .await?;
            if max_seen.as_ref().is_none_or(|m| m < &updated_at) {
                max_seen = Some(updated_at);
            }
        }
        if let Some(m) = max_seen {
            self.state.put_cursor(&scope, "pulls", &m).await?;
        }
        Ok(())
    }

    async fn poll_issue_comments(
        &self,
        r: &RepoRef,
        sink: &dyn EventSink,
    ) -> Result<(), HttpError> {
        let scope = r.slug();
        let cursor = self.state.get_cursor(&scope, "issue_comments").await?;
        let path = format!(
            "/repos/{}/{}/issues/comments?per_page=100&sort=updated&direction=asc{}",
            r.owner,
            r.name,
            Self::since_suffix(cursor.as_deref()),
        );
        let owner = r.owner.clone();
        let name = r.name.clone();
        let state = self.state.clone();

        let mut buffered: Vec<Value> = Vec::new();
        self.client
            .fetch_all(&path, |body| {
                if let Some(arr) = body.as_array() {
                    buffered.extend(arr.iter().cloned());
                }
                Ok(())
            })
            .await?;

        let mut max_seen = cursor.clone();
        for item in buffered {
            let updated_at = item
                .get("updated_at")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let id = item.get("id").and_then(Value::as_i64).unwrap_or_default();
            if id == 0 || updated_at.is_empty() {
                continue;
            }
            let issue_url = item
                .get("issue_url")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let issue_number = issue_number_from_url(issue_url).unwrap_or(0);
            if issue_number == 0 {
                continue;
            }
            let resource_key = format!("{owner}/{name}/issues/{issue_number}/{id}");
            let prior = state.get_seen("issue_comment", &resource_key).await?;
            if let Some((prev, _)) = &prior {
                if &updated_at <= prev {
                    continue;
                }
            }
            let class = classify_comment(prior.is_some());
            let payload = comment_payload(&owner, &name, issue_number, "issue", &item);
            let subject = issue_comment_subject(&owner, &name, issue_number, id);
            sink.publish(&subject, class.event_type, payload)
                .await
                .map_err(|e| HttpError::Status {
                    status: reqwest::StatusCode::INTERNAL_SERVER_ERROR,
                    body: format!("sink: {e}"),
                })?;
            state
                .put_seen("issue_comment", &resource_key, &updated_at, None)
                .await?;
            if max_seen.as_ref().is_none_or(|m| m < &updated_at) {
                max_seen = Some(updated_at);
            }
        }
        if let Some(m) = max_seen {
            self.state.put_cursor(&scope, "issue_comments", &m).await?;
        }
        Ok(())
    }

    async fn poll_pr_comments(&self, r: &RepoRef, sink: &dyn EventSink) -> Result<(), HttpError> {
        let scope = r.slug();
        let cursor = self.state.get_cursor(&scope, "pr_comments").await?;
        let path = format!(
            "/repos/{}/{}/pulls/comments?per_page=100&sort=updated&direction=asc{}",
            r.owner,
            r.name,
            Self::since_suffix(cursor.as_deref()),
        );
        let owner = r.owner.clone();
        let name = r.name.clone();
        let state = self.state.clone();

        let mut buffered: Vec<Value> = Vec::new();
        self.client
            .fetch_all(&path, |body| {
                if let Some(arr) = body.as_array() {
                    buffered.extend(arr.iter().cloned());
                }
                Ok(())
            })
            .await?;

        let mut max_seen = cursor.clone();
        for item in buffered {
            let updated_at = item
                .get("updated_at")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let id = item.get("id").and_then(Value::as_i64).unwrap_or_default();
            if id == 0 || updated_at.is_empty() {
                continue;
            }
            let pr_url = item
                .get("pull_request_url")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let pr_number = pr_number_from_url(pr_url).unwrap_or(0);
            if pr_number == 0 {
                continue;
            }
            let resource_key = format!("{owner}/{name}/pulls/{pr_number}/{id}");
            let prior = state.get_seen("pr_comment", &resource_key).await?;
            if let Some((prev, _)) = &prior {
                if &updated_at <= prev {
                    continue;
                }
            }
            let class = classify_comment(prior.is_some());
            let payload = comment_payload(&owner, &name, pr_number, "pull_request", &item);
            let subject = pr_comment_subject(&owner, &name, pr_number, id);
            sink.publish(&subject, class.event_type, payload)
                .await
                .map_err(|e| HttpError::Status {
                    status: reqwest::StatusCode::INTERNAL_SERVER_ERROR,
                    body: format!("sink: {e}"),
                })?;
            state
                .put_seen("pr_comment", &resource_key, &updated_at, None)
                .await?;
            if max_seen.as_ref().is_none_or(|m| m < &updated_at) {
                max_seen = Some(updated_at);
            }
        }
        if let Some(m) = max_seen {
            self.state.put_cursor(&scope, "pr_comments", &m).await?;
        }
        Ok(())
    }

    async fn poll_notifications(&self, sink: &dyn EventSink) -> Result<(), HttpError> {
        let cursor = self.state.get_cursor("user", "notifications").await?;
        let path = format!(
            "/notifications?all=true&per_page=50{}",
            Self::since_suffix(cursor.as_deref()),
        );

        let mut buffered: Vec<Value> = Vec::new();
        self.client
            .fetch_all(&path, |body| {
                if let Some(arr) = body.as_array() {
                    buffered.extend(arr.iter().cloned());
                }
                Ok(())
            })
            .await?;

        let mut max_seen = cursor.clone();
        for item in buffered {
            let id = item
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let updated_at = item
                .get("updated_at")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if id.is_empty() || updated_at.is_empty() {
                continue;
            }
            let resource_key = id.clone();
            let prior = self.state.get_seen("notification", &resource_key).await?;
            if let Some((prev, _)) = &prior {
                if &updated_at <= prev {
                    continue;
                }
            }
            let class = classify_notification();
            let payload = notification_payload(&item);
            let subject = notification_subject(&id);
            sink.publish(&subject, class.event_type, payload)
                .await
                .map_err(|e| HttpError::Status {
                    status: reqwest::StatusCode::INTERNAL_SERVER_ERROR,
                    body: format!("sink: {e}"),
                })?;
            self.state
                .put_seen("notification", &resource_key, &updated_at, None)
                .await?;
            if max_seen.as_ref().is_none_or(|m| m < &updated_at) {
                max_seen = Some(updated_at);
            }
        }
        if let Some(m) = max_seen {
            self.state.put_cursor("user", "notifications", &m).await?;
        }
        Ok(())
    }
}

/// Convert an [`HttpError`] into [`AdapterError`].
fn adapt_err(e: HttpError) -> AdapterError {
    AdapterError::Internal(format!("github: {e}"))
}
