//! OAuth2 device-code flow against Google's identity endpoints.
//!
//! Reference: <https://developers.google.com/identity/protocols/oauth2/limited-input-device>
//!
//! The flow:
//! 1. POST `client_id` + `scope` to the device-code endpoint. Receive a
//!    `device_code`, `user_code`, `verification_url`, `interval`, and
//!    `expires_in`.
//! 2. Print the verification URL + user code to stderr; the user opens
//!    the URL in a browser and types the code.
//! 3. POST `client_id` + `client_secret` + `device_code` +
//!    `grant_type=urn:ietf:params:oauth:grant-type:device_code` to the
//!    token endpoint, polling at the returned interval. Possible
//!    responses:
//!      - `authorization_pending` — keep polling.
//!      - `slow_down` — back off (interval += 5s).
//!      - `access_denied` — user declined; stop.
//!      - `expired_token` — device code expired; restart.
//!      - 200 with token payload — success; persist `refresh_token`.
//!
//! Refresh-token rotation: at runtime we POST
//! `grant_type=refresh_token` to obtain a fresh access token. We never
//! persist the access token — it is short-lived and re-fetched on each
//! `run` invocation.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Default Google device-code endpoint.
pub const DEFAULT_DEVICE_CODE_URL: &str = "https://oauth2.googleapis.com/device/code";

/// Default Google token endpoint.
pub const DEFAULT_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

/// Errors produced by the OAuth flow.
#[derive(Debug, thiserror::Error)]
pub enum OAuthError {
    /// Network or transport failure talking to Google.
    #[error("http error: {0}")]
    Http(String),

    /// Google returned a non-2xx response we don't know how to handle.
    #[error("oauth server returned {status}: {body}")]
    Server {
        /// HTTP status code.
        status: u16,
        /// Response body (may contain non-secret diagnostic info).
        body: String,
    },

    /// User declined authorization on the device-code consent screen.
    #[error("user denied authorization")]
    AccessDenied,

    /// The device code expired before the user completed authorization.
    #[error("device code expired before user completed authorization")]
    DeviceCodeExpired,

    /// We exceeded the device code's lifetime while polling.
    #[error("polling timed out after {0:?}")]
    PollingTimeout(Duration),

    /// Google returned a payload we couldn't parse.
    #[error("invalid response from oauth server: {0}")]
    InvalidResponse(String),
}

/// Configuration for the device-code flow.
#[derive(Debug, Clone)]
pub struct OAuthConfig {
    /// OAuth2 client ID (from Google Cloud Console).
    pub client_id: String,
    /// OAuth2 client secret (from Google Cloud Console).
    ///
    /// Stored only in memory while the flow is running; never logged.
    pub client_secret: String,
    /// Space-separated scope list. Default is the Gmail readonly scope.
    pub scope: String,
    /// Device-code endpoint URL.
    pub device_code_url: String,
    /// Token endpoint URL.
    pub token_url: String,
}

impl OAuthConfig {
    /// Build a config with default Google endpoints.
    pub fn google(client_id: String, client_secret: String, scope: String) -> Self {
        Self {
            client_id,
            client_secret,
            scope,
            device_code_url: DEFAULT_DEVICE_CODE_URL.to_string(),
            token_url: DEFAULT_TOKEN_URL.to_string(),
        }
    }
}

/// Response from the device-code endpoint.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct DeviceCodeResponse {
    /// Opaque device code; sent back to the token endpoint while polling.
    pub device_code: String,
    /// Short user-facing code displayed to the operator.
    pub user_code: String,
    /// URL the user opens in a browser to enter the user code.
    #[serde(alias = "verification_uri")]
    pub verification_url: String,
    /// Lifetime of the device code, in seconds.
    pub expires_in: u64,
    /// Recommended polling interval, in seconds.
    pub interval: u64,
}

/// Subset of the token-endpoint success response we care about.
#[derive(Debug, Deserialize, Clone)]
pub struct TokenResponse {
    /// Short-lived access token.
    pub access_token: String,
    /// Long-lived refresh token. Only returned the first time the user
    /// authorizes the client.
    pub refresh_token: Option<String>,
    /// Lifetime of the access token in seconds (typically 3600).
    pub expires_in: u64,
    /// Token type — always `Bearer` for Google.
    #[serde(default)]
    pub token_type: Option<String>,
    /// Scope actually granted (may be a subset of what we requested).
    #[serde(default)]
    pub scope: Option<String>,
}

/// Token-endpoint error response (used while polling).
#[derive(Debug, Deserialize)]
struct TokenErrorResponse {
    error: String,
}

/// Result of a successful device-code flow.
#[derive(Debug, Clone)]
pub struct AuthorizedTokens {
    /// Refresh token to persist (encrypted) on disk.
    pub refresh_token: String,
    /// Access token to use for the very first sync (we'll refresh later).
    pub access_token: String,
    /// When the access token expires (monotonic, computed from
    /// `expires_in`).
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

/// Request a device code from Google.
pub async fn request_device_code(
    client: &reqwest::Client,
    config: &OAuthConfig,
) -> Result<DeviceCodeResponse, OAuthError> {
    let resp = client
        .post(&config.device_code_url)
        .form(&[
            ("client_id", config.client_id.as_str()),
            ("scope", config.scope.as_str()),
        ])
        .send()
        .await
        .map_err(|e| OAuthError::Http(e.to_string()))?;

    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| OAuthError::Http(e.to_string()))?;

    if !status.is_success() {
        return Err(OAuthError::Server {
            status: status.as_u16(),
            body,
        });
    }

    serde_json::from_str::<DeviceCodeResponse>(&body)
        .map_err(|e| OAuthError::InvalidResponse(e.to_string()))
}

/// Poll the token endpoint until the user authorizes (or we time out).
///
/// `now_fn` returns the current instant; it's a parameter so tests can
/// drive the clock without sleeping. `sleep_fn` performs the inter-poll
/// wait — likewise injectable for tests.
pub async fn poll_for_tokens(
    client: &reqwest::Client,
    config: &OAuthConfig,
    code: &DeviceCodeResponse,
) -> Result<AuthorizedTokens, OAuthError> {
    let started = Instant::now();
    let lifetime = Duration::from_secs(code.expires_in);
    let mut interval = Duration::from_secs(code.interval.max(1));

    loop {
        if started.elapsed() >= lifetime {
            return Err(OAuthError::PollingTimeout(lifetime));
        }

        tokio::time::sleep(interval).await;

        let resp = client
            .post(&config.token_url)
            .form(&[
                ("client_id", config.client_id.as_str()),
                ("client_secret", config.client_secret.as_str()),
                ("device_code", code.device_code.as_str()),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ])
            .send()
            .await
            .map_err(|e| OAuthError::Http(e.to_string()))?;

        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| OAuthError::Http(e.to_string()))?;

        if status.is_success() {
            let tokens: TokenResponse = serde_json::from_str(&body)
                .map_err(|e| OAuthError::InvalidResponse(e.to_string()))?;
            let refresh_token = tokens.refresh_token.clone().ok_or_else(|| {
                OAuthError::InvalidResponse(
                    "token endpoint returned success but no refresh_token; \
                     re-create the OAuth client with access_type=offline"
                        .to_string(),
                )
            })?;
            let expires_at = chrono::Utc::now()
                + chrono::Duration::seconds(tokens.expires_in.min(i64::MAX as u64) as i64);
            return Ok(AuthorizedTokens {
                refresh_token,
                access_token: tokens.access_token,
                expires_at,
            });
        }

        // Non-2xx: check the OAuth error code to decide whether to keep
        // polling, slow down, or bail.
        let err: TokenErrorResponse = match serde_json::from_str(&body) {
            Ok(e) => e,
            Err(_) => {
                return Err(OAuthError::Server {
                    status: status.as_u16(),
                    body,
                });
            }
        };

        match err.error.as_str() {
            "authorization_pending" => {
                // keep polling at the current interval
            }
            "slow_down" => {
                interval += Duration::from_secs(5);
            }
            "access_denied" => return Err(OAuthError::AccessDenied),
            "expired_token" => return Err(OAuthError::DeviceCodeExpired),
            other => {
                return Err(OAuthError::Server {
                    status: status.as_u16(),
                    body: format!("oauth error code: {other}"),
                });
            }
        }
    }
}

/// Use a refresh token to obtain a fresh access token.
pub async fn refresh_access_token(
    client: &reqwest::Client,
    config: &OAuthConfig,
    refresh_token: &str,
) -> Result<AuthorizedTokens, OAuthError> {
    let resp = client
        .post(&config.token_url)
        .form(&[
            ("client_id", config.client_id.as_str()),
            ("client_secret", config.client_secret.as_str()),
            ("refresh_token", refresh_token),
            ("grant_type", "refresh_token"),
        ])
        .send()
        .await
        .map_err(|e| OAuthError::Http(e.to_string()))?;

    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| OAuthError::Http(e.to_string()))?;

    if !status.is_success() {
        return Err(OAuthError::Server {
            status: status.as_u16(),
            body,
        });
    }

    let tokens: TokenResponse =
        serde_json::from_str(&body).map_err(|e| OAuthError::InvalidResponse(e.to_string()))?;

    // refresh_token grants don't return a new refresh_token; preserve
    // the one we already have.
    let refresh_token_out = tokens
        .refresh_token
        .clone()
        .unwrap_or_else(|| refresh_token.to_string());
    let expires_at = chrono::Utc::now()
        + chrono::Duration::seconds(tokens.expires_in.min(i64::MAX as u64) as i64);
    Ok(AuthorizedTokens {
        refresh_token: refresh_token_out,
        access_token: tokens.access_token,
        expires_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_default_endpoints() {
        let cfg = OAuthConfig::google("id".to_string(), "secret".to_string(), "scope".to_string());
        assert_eq!(cfg.device_code_url, DEFAULT_DEVICE_CODE_URL);
        assert_eq!(cfg.token_url, DEFAULT_TOKEN_URL);
    }

    #[test]
    fn device_code_response_deserializes_with_verification_uri_alias() {
        // Google's actual response uses `verification_url` but some
        // device-flow clients use `verification_uri`. We accept both.
        let body = r#"{
            "device_code": "DC",
            "user_code": "UC",
            "verification_uri": "https://example.com/device",
            "expires_in": 1800,
            "interval": 5
        }"#;
        let parsed: DeviceCodeResponse = serde_json::from_str(body).unwrap();
        assert_eq!(parsed.verification_url, "https://example.com/device");
    }
}
