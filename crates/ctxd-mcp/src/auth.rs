//! Token-resolution layer shared across stdio, SSE, and streamable-HTTP transports.
//!
//! The MCP tool surface accepts capability tokens via two channels:
//!
//! 1. The per-tool-call `token` argument (the original stdio convention).
//! 2. An `Authorization: Bearer <base64-biscuit>` HTTP header (preferred for
//!    SSE and streamable-HTTP).
//!
//! When both are present, **the header wins** and the tool argument is
//! silently ignored. That precedence is enforced upstream of the tool
//! handlers — the HTTP middleware in [`crate::transport`] rewrites the
//! incoming JSON-RPC body to substitute `params.arguments.token` with the
//! header token before dispatching the request to the rmcp service.
//!
//! This module exposes two pieces:
//!
//! * [`extract_bearer_token`] — parse a single `Authorization` header
//!   value, returning `Some(token_str)` only when it begins with the
//!   `Bearer ` (ASCII, case-sensitive per [RFC 6750]) prefix and the
//!   token body is itself ASCII. Non-ASCII bytes are rejected to defend
//!   against header injection.
//! * [`AuthPolicy`] — a tiny configuration knob describing whether the
//!   transport requires authentication. Stdio defaults to
//!   [`AuthPolicy::Optional`] (legacy behaviour); HTTP transports may opt
//!   into [`AuthPolicy::Required`] via the `--require-auth` CLI flag.
//!
//! [RFC 6750]: https://datatracker.ietf.org/doc/html/rfc6750#section-2.1

/// Whether a transport requires every tool call to carry a verifiable token.
///
/// `Optional` is the v0.1/v0.2 stdio default — a tool call with no token is
/// allowed and the cap engine is consulted only when a token is supplied.
/// `Required` is the v0.3 hardening mode — every call must present a valid
/// token, either as an `Authorization: Bearer` header (HTTP transports) or
/// as a `token` argument (stdio). Calls without one are rejected before
/// they reach the tool handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthPolicy {
    /// Tokens are checked when present; their absence is not an error.
    #[default]
    Optional,
    /// Every tool call must present a token. Missing tokens are rejected.
    Required,
}

impl AuthPolicy {
    /// Returns `true` when this policy demands every call carry a token.
    pub fn is_required(self) -> bool {
        matches!(self, AuthPolicy::Required)
    }
}

/// Extract a base64-encoded biscuit token from an `Authorization` header
/// value.
///
/// Accepts only `Bearer <token>`, case-sensitive on the scheme prefix per
/// RFC 6750 §2.1. Non-ASCII bytes anywhere in the value cause the header
/// to be rejected (`None`) — this protects downstream consumers from
/// surprises like UTF-8 homoglyph attacks or smuggled CR/LF.
///
/// The returned slice is a borrow into the original header value; it does
/// not include the `Bearer ` prefix or any surrounding whitespace.
///
/// # Examples
///
/// ```
/// use ctxd_mcp::auth::extract_bearer_token;
///
/// assert_eq!(
///     extract_bearer_token("Bearer abc123"),
///     Some("abc123".to_string()),
/// );
/// assert_eq!(extract_bearer_token("bearer abc123"), None);
/// assert_eq!(extract_bearer_token("Basic xyz"), None);
/// assert_eq!(extract_bearer_token("Bearer "), None);
/// ```
pub fn extract_bearer_token(value: &str) -> Option<String> {
    if !value.is_ascii() {
        // Defend against header injection: refuse to look at anything that
        // isn't pure ASCII. CR/LF/NUL are caught here as a side effect.
        return None;
    }
    // Case-sensitive `Bearer ` prefix (RFC 6750 is conventionally written
    // that way, though some implementations are lenient — we choose the
    // strict interpretation here so logging is deterministic and
    // off-by-one casing bugs in clients fail loudly).
    let token = value.strip_prefix("Bearer ")?;
    let trimmed = token.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Reject embedded whitespace inside the token itself — biscuits are
    // base64 so they should never contain spaces.
    if trimmed.split_whitespace().count() != 1 {
        return None;
    }
    Some(trimmed.to_string())
}

/// Resolve the effective token for a tool call given the header token (if
/// any, already extracted by middleware) and the per-call `token` argument
/// (if any, from the JSON-RPC body).
///
/// This is the single place that codifies the precedence rule documented
/// in the module header: **header wins over arg**. When both are present,
/// `arg_token` is ignored.
///
/// Returns `None` only when neither source provides a token. Whether that
/// is an error depends on the active [`AuthPolicy`].
pub fn resolve_token(header_token: Option<&str>, arg_token: Option<&str>) -> Option<String> {
    if let Some(h) = header_token {
        return Some(h.to_string());
    }
    arg_token.map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_simple() {
        assert_eq!(extract_bearer_token("Bearer abc"), Some("abc".to_string()));
    }

    #[test]
    fn bearer_with_trailing_whitespace() {
        // We trim the token body but require the prefix to be exactly "Bearer ".
        assert_eq!(
            extract_bearer_token("Bearer abc   "),
            Some("abc".to_string())
        );
    }

    #[test]
    fn bearer_empty_token_rejected() {
        assert_eq!(extract_bearer_token("Bearer "), None);
        assert_eq!(extract_bearer_token("Bearer    "), None);
    }

    #[test]
    fn bearer_lowercase_rejected() {
        assert_eq!(extract_bearer_token("bearer abc"), None);
    }

    #[test]
    fn other_schemes_rejected() {
        assert_eq!(extract_bearer_token("Basic xyz"), None);
        assert_eq!(extract_bearer_token("Token abc"), None);
    }

    #[test]
    fn non_ascii_rejected() {
        // U+200B zero-width space — the kind of thing header injection
        // payloads love. We refuse to even look at it.
        assert_eq!(extract_bearer_token("Bearer ab\u{200B}c"), None);
    }

    #[test]
    fn embedded_whitespace_rejected() {
        // Defence against "Bearer abc def" being smuggled through.
        assert_eq!(extract_bearer_token("Bearer abc def"), None);
    }

    #[test]
    fn resolve_prefers_header() {
        assert_eq!(
            resolve_token(Some("hdr"), Some("arg")),
            Some("hdr".to_string())
        );
    }

    #[test]
    fn resolve_falls_back_to_arg() {
        assert_eq!(resolve_token(None, Some("arg")), Some("arg".to_string()));
    }

    #[test]
    fn resolve_neither() {
        assert_eq!(resolve_token(None, None), None);
    }

    #[test]
    fn policy_default_is_optional() {
        assert_eq!(AuthPolicy::default(), AuthPolicy::Optional);
        assert!(!AuthPolicy::default().is_required());
        assert!(AuthPolicy::Required.is_required());
    }
}
