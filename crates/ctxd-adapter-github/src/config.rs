//! Configuration types for the GitHub adapter.

use std::path::PathBuf;
use std::time::Duration;

/// Where the adapter should look for repos to poll.
#[derive(Debug, Clone)]
pub enum RepoSelector {
    /// An explicit list of `owner/name` repos.
    Explicit(Vec<RepoRef>),
    /// All repos accessible to the authenticated user (via `/user/repos`).
    AuthenticatedUser,
}

/// A specific repository to poll.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RepoRef {
    /// Repo owner (user or org).
    pub owner: String,
    /// Repo name.
    pub name: String,
}

impl RepoRef {
    /// Parse an `owner/name` string into a [`RepoRef`].
    pub fn parse(s: &str) -> Result<Self, String> {
        let mut parts = s.splitn(2, '/');
        let owner = parts
            .next()
            .filter(|p| !p.is_empty())
            .ok_or_else(|| format!("invalid repo (missing owner): {s}"))?;
        let name = parts
            .next()
            .filter(|p| !p.is_empty())
            .ok_or_else(|| format!("invalid repo (expected owner/name): {s}"))?;
        Ok(Self {
            owner: owner.to_string(),
            name: name.to_string(),
        })
    }

    /// Format as `owner/name`.
    pub fn slug(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

/// Which kinds of GitHub resources to poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceKind {
    /// Issues (excluding PRs).
    Issues,
    /// Pull requests.
    Pulls,
    /// Issue + PR review comments.
    Comments,
    /// User notification feed.
    Notifications,
}

impl ResourceKind {
    /// Parse a single kind name.
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim() {
            "issues" => Ok(Self::Issues),
            "pulls" | "prs" => Ok(Self::Pulls),
            "comments" => Ok(Self::Comments),
            "notifications" => Ok(Self::Notifications),
            other => Err(format!("unknown kind: {other}")),
        }
    }

    /// All kinds, in a stable order.
    pub fn all() -> &'static [ResourceKind] {
        &[
            Self::Issues,
            Self::Pulls,
            Self::Comments,
            Self::Notifications,
        ]
    }

    /// Short string name (used for state-DB keys + tracing).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Issues => "issues",
            Self::Pulls => "pulls",
            Self::Comments => "comments",
            Self::Notifications => "notifications",
        }
    }
}

/// Resolved adapter configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// GitHub API base URL. Defaults to `https://api.github.com`. Tests
    /// override this to point at a `wiremock` mock server.
    pub api_base: String,
    /// Personal access token (never logged).
    pub token: String,
    /// Which repos to poll.
    pub repos: RepoSelector,
    /// Path to the state DB.
    pub state_dir: PathBuf,
    /// How long to sleep between polling cycles.
    pub poll_interval: Duration,
    /// Resource kinds to poll.
    pub kinds: Vec<ResourceKind>,
    /// Whether to poll `/notifications` for the authenticated user. Honoured
    /// only when [`ResourceKind::Notifications`] is in `kinds`.
    pub include_notifications: bool,
    /// Maximum number of polling cycles to run before returning. `None` means
    /// run forever; tests use `Some(1)` for a single pass.
    pub max_cycles: Option<u32>,
}

impl Config {
    /// Returns true if the given kind is enabled.
    pub fn has_kind(&self, k: ResourceKind) -> bool {
        self.kinds.contains(&k)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_repo_ok() {
        let r = RepoRef::parse("acme/web").expect("ok");
        assert_eq!(r.owner, "acme");
        assert_eq!(r.name, "web");
        assert_eq!(r.slug(), "acme/web");
    }

    #[test]
    fn parse_repo_missing_owner() {
        assert!(RepoRef::parse("/web").is_err());
        assert!(RepoRef::parse("acme/").is_err());
        assert!(RepoRef::parse("noslash").is_err());
    }

    #[test]
    fn kinds_parse() {
        assert_eq!(ResourceKind::parse("issues").unwrap(), ResourceKind::Issues);
        assert_eq!(ResourceKind::parse("prs").unwrap(), ResourceKind::Pulls);
        assert_eq!(ResourceKind::parse("pulls").unwrap(), ResourceKind::Pulls);
        assert!(ResourceKind::parse("garbage").is_err());
    }

    #[test]
    fn kinds_str_round_trip() {
        for k in ResourceKind::all() {
            let s = k.as_str();
            let parsed = ResourceKind::parse(s).expect("round-trip");
            assert_eq!(parsed, *k);
        }
    }
}
