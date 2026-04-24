//! Axum middleware shared by the SSE and streamable-HTTP transports.
//!
//! This middleware is the *only* place where the
//! "header beats arg" precedence rule for capability tokens is
//! enforced — see [`crate::auth`] for the policy itself.
//!
//! ## What it does, in order
//!
//! 1. Extract the `Authorization` header (if any) into a sanitised
//!    bearer token via [`crate::auth::extract_bearer_token`]. Malformed
//!    or non-ASCII headers are dropped silently — we do not 401 on a
//!    bad header because the [`AuthPolicy`] check below is what decides
//!    whether to reject.
//! 2. Buffer the request body up to [`super::DEFAULT_MAX_BODY_BYTES`].
//!    Larger payloads return 413 Payload Too Large — DoS defence.
//! 3. If the body parses as a JSON-RPC `tools/call` request, rewrite
//!    `params.arguments.token` to the header token (when the header is
//!    present). Other shapes (`initialize`, `tools/list`,
//!    notifications, batches we don't recognise) pass through
//!    unmodified.
//! 4. Apply the [`AuthPolicy`]: when `Required` and the call is a
//!    `tools/call` with no token reachable from either source, return
//!    401 Unauthorized.
//! 5. Forward the (possibly rewritten) request downstream.
//!
//! Tracing: emits a single INFO line per request with `remote_addr`,
//! `method`, and `tool_name` (when discoverable). Header values and
//! token bytes are **never** logged.

use crate::auth::{extract_bearer_token, AuthPolicy};
use axum::{
    body::{to_bytes, Body},
    extract::State,
    http::{HeaderMap, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::net::SocketAddr;

/// Per-transport configuration the middleware needs at request time.
#[derive(Debug, Clone)]
pub struct AuthMiddlewareConfig {
    /// Whether to reject `tools/call` requests that present no token.
    pub policy: AuthPolicy,
    /// Maximum allowed JSON-RPC body size in bytes.
    pub max_body_bytes: usize,
}

impl AuthMiddlewareConfig {
    /// Construct a config with the given policy and the default body limit.
    pub fn new(policy: AuthPolicy) -> Self {
        Self {
            policy,
            max_body_bytes: super::DEFAULT_MAX_BODY_BYTES,
        }
    }
}

/// Axum middleware function: applies header-token precedence, body-size
/// limits, and require-auth enforcement, then forwards.
pub async fn auth_layer(
    State(cfg): State<AuthMiddlewareConfig>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let remote_addr = req
        .extensions()
        .get::<axum::extract::ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0.to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let header_token = bearer_from_headers(req.headers());

    // Only POSTs carry JSON-RPC bodies we might need to inspect /
    // rewrite. GETs (SSE streams) and DELETEs (session teardown) skip
    // the body work.
    let is_post = req.method() == axum::http::Method::POST;
    let request_method = req.method().clone();

    let (parts, body) = req.into_parts();

    let (forward_body, tool_name, body_token_present) = if is_post {
        let bytes = match to_bytes(body, cfg.max_body_bytes).await {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(
                    remote_addr = %remote_addr,
                    error = %e,
                    "rejecting request: body too large or unreadable"
                );
                return (
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "Payload too large or unreadable",
                )
                    .into_response();
            }
        };
        let (new_bytes, tool, body_token) =
            rewrite_token_in_jsonrpc(&bytes, header_token.as_deref());
        (Body::from(new_bytes), tool, body_token)
    } else {
        (body, None, false)
    };

    let token_present = header_token.is_some() || body_token_present;
    let is_tool_call = tool_name.is_some();

    if cfg.policy.is_required() && is_tool_call && !token_present {
        tracing::info!(
            remote_addr = %remote_addr,
            method = %request_method,
            tool_name = ?tool_name,
            "rejecting unauthenticated tool call (require-auth on)"
        );
        return (StatusCode::UNAUTHORIZED, "Missing capability token").into_response();
    }

    tracing::info!(
        remote_addr = %remote_addr,
        method = %request_method,
        tool_name = ?tool_name,
        "MCP HTTP request"
    );

    let req = Request::from_parts(parts, forward_body);
    next.run(req).await
}

/// Pull the bearer token (if any) out of an `Authorization` header.
///
/// Multiple `Authorization` headers, malformed encodings, or non-ASCII
/// payloads all return `None`. The header value is **never** logged.
fn bearer_from_headers(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(axum::http::header::AUTHORIZATION)?;
    let s = value.to_str().ok()?;
    extract_bearer_token(s)
}

/// Inspect a JSON-RPC request body. Returns the (possibly re-encoded)
/// bytes, the visible tool name (when the message is a `tools/call`),
/// and whether a non-empty `token` argument was already present in the
/// body.
///
/// When `header_token` is `Some`, the token field of any
/// `tools/call` `params.arguments` is replaced by the header value
/// (header beats arg, per the documented precedence rule).
///
/// On any parse error we return the input bytes unchanged with `None`
/// tool name and `false` body-token — downstream rmcp will produce a
/// JSON-RPC error response.
fn rewrite_token_in_jsonrpc(
    bytes: &[u8],
    header_token: Option<&str>,
) -> (Vec<u8>, Option<String>, bool) {
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return (bytes.to_vec(), None, false);
    };

    let tool_name = extract_tool_name(&value);
    let body_token_present = json_has_token_arg(&value);

    if let Some(header_tok) = header_token {
        let mut mutated = false;
        match &mut value {
            serde_json::Value::Array(items) => {
                for item in items.iter_mut() {
                    if inject_token(item, header_tok) {
                        mutated = true;
                    }
                }
            }
            obj @ serde_json::Value::Object(_) => {
                if inject_token(obj, header_tok) {
                    mutated = true;
                }
            }
            _ => {}
        }
        if !mutated {
            // Header was supplied but the body wasn't a `tools/call`
            // we know how to rewrite — pass through verbatim so we
            // don't perturb byte-for-byte equality the caller may
            // depend on (e.g. signed body envelopes).
            return (bytes.to_vec(), tool_name, body_token_present);
        }
        let bytes = match serde_json::to_vec(&value) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "failed to re-serialize rewritten body");
                return (bytes.to_vec(), tool_name, body_token_present);
            }
        };
        (bytes, tool_name, body_token_present)
    } else {
        (bytes.to_vec(), tool_name, body_token_present)
    }
}

fn extract_tool_name(value: &serde_json::Value) -> Option<String> {
    let obj = value.as_object()?;
    let method = obj.get("method")?.as_str()?;
    if method != "tools/call" {
        return None;
    }
    let params = obj.get("params")?.as_object()?;
    let name = params.get("name")?.as_str()?;
    Some(name.to_string())
}

/// Inject `header_token` into a JSON-RPC object's
/// `params.arguments.token` field. Returns `true` when a mutation
/// actually happened, `false` otherwise.
fn inject_token(value: &mut serde_json::Value, header_token: &str) -> bool {
    let Some(obj) = value.as_object_mut() else {
        return false;
    };
    if obj.get("method").and_then(|v| v.as_str()) != Some("tools/call") {
        return false;
    }
    let Some(params) = obj.get_mut("params") else {
        return false;
    };
    let Some(params_obj) = params.as_object_mut() else {
        return false;
    };
    let arguments = params_obj
        .entry("arguments")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    if let Some(args_obj) = arguments.as_object_mut() {
        // Header wins: clobber any existing token field.
        args_obj.insert(
            "token".to_string(),
            serde_json::Value::String(header_token.to_string()),
        );
        return true;
    }
    false
}

fn json_has_token_arg(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Array(items) => items.iter().any(json_has_token_arg),
        serde_json::Value::Object(obj) => {
            if obj.get("method").and_then(|v| v.as_str()) == Some("tools/call") {
                obj.get("params")
                    .and_then(|p| p.as_object())
                    .and_then(|p| p.get("arguments"))
                    .and_then(|a| a.as_object())
                    .and_then(|a| a.get("token"))
                    .and_then(|t| t.as_str())
                    .is_some_and(|s| !s.is_empty())
            } else {
                false
            }
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrite_replaces_token_on_tools_call() {
        let body = br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"ctx_read","arguments":{"subject":"/x","token":"argtok"}}}"#;
        let (out, name, body_token) = rewrite_token_in_jsonrpc(body, Some("hdrtok"));
        assert_eq!(name.as_deref(), Some("ctx_read"));
        assert!(body_token);
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["params"]["arguments"]["token"], "hdrtok");
    }

    #[test]
    fn rewrite_inserts_token_when_args_missing_token_field() {
        let body = br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"ctx_read","arguments":{"subject":"/x"}}}"#;
        let (out, _, body_token) = rewrite_token_in_jsonrpc(body, Some("hdrtok"));
        assert!(!body_token);
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["params"]["arguments"]["token"], "hdrtok");
    }

    #[test]
    fn rewrite_leaves_other_methods_alone() {
        let body = br#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#;
        let (out, name, body_token) = rewrite_token_in_jsonrpc(body, Some("hdrtok"));
        assert!(name.is_none());
        assert!(!body_token);
        assert_eq!(out, body);
    }

    #[test]
    fn rewrite_handles_invalid_json() {
        let body = b"not json at all";
        let (out, name, body_token) = rewrite_token_in_jsonrpc(body, Some("hdrtok"));
        assert_eq!(out, body);
        assert!(name.is_none());
        assert!(!body_token);
    }

    #[test]
    fn rewrite_no_header_passes_through() {
        let body = br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"ctx_read","arguments":{"subject":"/x","token":"argtok"}}}"#;
        let (out, name, body_token) = rewrite_token_in_jsonrpc(body, None);
        assert_eq!(out, body);
        assert_eq!(name.as_deref(), Some("ctx_read"));
        assert!(body_token);
    }

    #[test]
    fn json_has_token_detects_present() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"method":"tools/call","params":{"name":"ctx_read","arguments":{"token":"x"}}}"#,
        )
        .unwrap();
        assert!(json_has_token_arg(&v));
    }

    #[test]
    fn json_has_token_detects_absent() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"method":"tools/call","params":{"name":"ctx_read","arguments":{}}}"#,
        )
        .unwrap();
        assert!(!json_has_token_arg(&v));
    }
}
