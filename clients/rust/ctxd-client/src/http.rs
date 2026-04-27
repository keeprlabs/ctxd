//! HTTP admin client for the ctxd daemon.
//!
//! Hand-written types matching `docs/api/openapi.yaml`. Codegen
//! (progenitor, openapi-generator, etc.) is intentionally avoided for
//! v0.3 — the surface is small, the shapes are stable, and a
//! hand-rolled client is easier to read in an incident.
//!
//! Auth model: every constructor accepts an optional bearer token.
//! When set, it is attached as `Authorization: Bearer <token>` on
//! every request. Endpoints documented as open (`/health`, `/v1/grant`,
//! `/v1/stats`, `/v1/approvals`) tolerate the header being present
//! anyway, so we send it unconditionally when configured.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::errors::CtxdError;

/// Default timeout for individual HTTP admin calls. Picked to be long
/// enough to forgive a slow loopback under contention but short enough
/// that a misconfigured URL fails the SDK loudly rather than hanging
/// the host application.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// HTTP admin client.
#[derive(Debug, Clone)]
pub struct HttpAdminClient {
    base: String,
    inner: reqwest::Client,
    token: Option<String>,
}

impl HttpAdminClient {
    /// Construct a client pointed at the given base URL.
    ///
    /// The URL must include the scheme and host (e.g.
    /// `http://127.0.0.1:7777`). Trailing slashes are stripped so
    /// callers can pass either form.
    pub fn new(base_url: &str) -> Result<Self, CtxdError> {
        let inner = reqwest::Client::builder()
            .timeout(DEFAULT_TIMEOUT)
            .build()?;
        Ok(Self {
            base: base_url.trim_end_matches('/').to_string(),
            inner,
            token: None,
        })
    }

    /// Attach a capability token. Sent as `Authorization: Bearer
    /// <token>` on every subsequent call.
    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    /// Issue a GET against `<base>/<path>` and decode the JSON body.
    async fn get_json<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T, CtxdError> {
        let url = format!("{}{}", self.base, path);
        let mut req = self.inner.get(&url);
        if let Some(t) = &self.token {
            req = req.bearer_auth(t);
        }
        let resp = req.send().await?;
        let status = resp.status();
        if status.is_success() {
            let body = resp.json::<T>().await?;
            Ok(body)
        } else {
            let body = resp.text().await.unwrap_or_default();
            Err(CtxdError::http_status(status, body))
        }
    }

    /// Issue a POST with a JSON body and decode the JSON response.
    async fn post_json<B: Serialize, T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, CtxdError> {
        let url = format!("{}{}", self.base, path);
        let mut req = self.inner.post(&url).json(body);
        if let Some(t) = &self.token {
            req = req.bearer_auth(t);
        }
        let resp = req.send().await?;
        let status = resp.status();
        if status.is_success() {
            let body = resp.json::<T>().await?;
            Ok(body)
        } else {
            let body = resp.text().await.unwrap_or_default();
            Err(CtxdError::http_status(status, body))
        }
    }

    /// Issue a DELETE; expects a 204 No Content on success.
    async fn delete_no_content(&self, path: &str) -> Result<(), CtxdError> {
        let url = format!("{}{}", self.base, path);
        let mut req = self.inner.delete(&url);
        if let Some(t) = &self.token {
            req = req.bearer_auth(t);
        }
        let resp = req.send().await?;
        let status = resp.status();
        if status.is_success() {
            Ok(())
        } else {
            let body = resp.text().await.unwrap_or_default();
            Err(CtxdError::http_status(status, body))
        }
    }

    /// `GET /health` — daemon liveness + version probe.
    pub async fn health(&self) -> Result<HealthInfo, CtxdError> {
        self.get_json("/health").await
    }

    /// `GET /v1/stats` — basic store statistics.
    pub async fn stats(&self) -> Result<StatsInfo, CtxdError> {
        self.get_json("/v1/stats").await
    }

    /// `POST /v1/grant` — mint a capability token.
    ///
    /// Returns the base64-encoded token (the `token` field of the
    /// daemon's response).
    pub async fn grant(
        &self,
        subject: &str,
        operations: &[Operation],
        expires_at: Option<DateTime<Utc>>,
    ) -> Result<String, CtxdError> {
        let expires_in_secs = expires_at.map(|t| {
            let now = Utc::now();
            let delta = t.signed_duration_since(now).num_seconds();
            // The daemon requires a positive integer; clamp at 1s if a
            // caller hands us a past timestamp so we surface a clear
            // 400 from the server rather than a confusing
            // "expires_in_secs: 0" rejection.
            delta.max(1)
        });
        let body = GrantRequest {
            subject: subject.to_string(),
            operations: operations.iter().map(|o| o.as_wire_str().to_string()).collect(),
            expires_in_secs,
        };
        let resp: GrantResponse = self.post_json("/v1/grant", &body).await?;
        Ok(resp.token)
    }

    /// Revoke a capability token by id.
    ///
    /// **Note:** the daemon does not yet expose a REST revoke endpoint
    /// in v0.3. The wire-protocol `Revoke` verb is the supported path.
    /// This HTTP-level method exists for shape parity with the wire
    /// client; today it returns
    /// [`CtxdError::HttpStatus`] with status `405` so callers can
    /// detect-and-fall-back. Tracked for v0.4.
    pub async fn revoke(&self, _token_id: &str) -> Result<(), CtxdError> {
        Err(CtxdError::HttpStatus {
            status: 405,
            body: "HTTP revoke is not implemented; use the wire protocol Revoke verb".to_string(),
        })
    }

    /// `GET /v1/peers` — list federation peers (admin).
    pub async fn peers(&self) -> Result<Vec<PeerInfo>, CtxdError> {
        let resp: PeerListResponse = self.get_json("/v1/peers").await?;
        Ok(resp.peers)
    }

    /// `DELETE /v1/peers/{peer_id}` — remove a federation peer (admin).
    pub async fn peer_remove(&self, peer_id: &str) -> Result<(), CtxdError> {
        let path = format!("/v1/peers/{peer_id}");
        self.delete_no_content(&path).await
    }
}

/// Operations a capability token can authorize.
///
/// Mirrors `ctxd_cap::Operation` and the OpenAPI spec's
/// `operations[]` enum. Wire serialization matches the daemon's
/// snake_case names exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Operation {
    /// Read events under a subject.
    Read,
    /// Write (append) events.
    Write,
    /// List subject paths.
    Subjects,
    /// FTS / vector search.
    Search,
    /// Admin operations (mint tokens, manage peers).
    Admin,
}

impl Operation {
    /// Wire-format string used in the JSON body of `/v1/grant` and
    /// the wire-protocol `Grant` verb.
    pub fn as_wire_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Subjects => "subjects",
            Self::Search => "search",
            Self::Admin => "admin",
        }
    }
}

impl std::fmt::Display for Operation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_wire_str())
    }
}

/// `GET /health` response body.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HealthInfo {
    /// Always `"ok"` on a healthy daemon.
    pub status: String,
    /// Daemon package version (semver, e.g. `"0.3.0"`).
    pub version: String,
}

/// `GET /v1/stats` response body. Future versions of the daemon may
/// add fields; the SDK accepts unknown fields silently.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StatsInfo {
    /// Number of distinct subjects with at least one event.
    pub subject_count: i64,
}

/// `GET /v1/peers` response item.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PeerInfo {
    /// Local identifier for this peer.
    pub peer_id: String,
    /// Address we dial when replicating with this peer.
    pub url: String,
    /// Remote peer's Ed25519 public key, hex-encoded lowercase.
    pub public_key: String,
    /// Subject globs we are willing to deliver to this peer.
    pub subject_patterns: Vec<String>,
    /// RFC3339 timestamp the peer was first registered.
    pub added_at: String,
    /// RFC3339 timestamp of the last successful exchange. Reserved
    /// for v0.4; always `None` today.
    #[serde(default)]
    pub last_seen_at: Option<String>,
}

#[derive(Serialize)]
struct GrantRequest {
    subject: String,
    operations: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expires_in_secs: Option<i64>,
}

#[derive(Deserialize)]
struct GrantResponse {
    token: String,
}

#[derive(Deserialize)]
struct PeerListResponse {
    peers: Vec<PeerInfo>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_strings_match_wire() {
        assert_eq!(Operation::Read.as_wire_str(), "read");
        assert_eq!(Operation::Write.as_wire_str(), "write");
        assert_eq!(Operation::Subjects.as_wire_str(), "subjects");
        assert_eq!(Operation::Search.as_wire_str(), "search");
        assert_eq!(Operation::Admin.as_wire_str(), "admin");
    }

    #[test]
    fn base_url_strips_trailing_slash() {
        let c = HttpAdminClient::new("http://localhost:7777/").expect("new");
        assert_eq!(c.base, "http://localhost:7777");
        let c = HttpAdminClient::new("http://localhost:7777").expect("new");
        assert_eq!(c.base, "http://localhost:7777");
    }

    #[test]
    fn with_token_attaches_token() {
        let c = HttpAdminClient::new("http://localhost:7777")
            .expect("new")
            .with_token("Y2Fw");
        assert_eq!(c.token.as_deref(), Some("Y2Fw"));
    }
}
