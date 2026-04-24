//! Gmail adapter for ctxd.
//!
//! Implements OAuth2 device-code authorization and AES-256-GCM at-rest
//! token encryption. Gmail API sync, the [`Adapter`] trait
//! implementation, and the `ctxd-adapter-gmail` binary land in
//! follow-up commits.
//!
//! # Modules
//! - [`oauth`] — OAuth2 device-code flow + token refresh.
//! - [`crypto`] — AES-256-GCM token encryption at rest.

pub mod crypto;
pub mod oauth;

/// OAuth2 scope required for the adapter (read-only Gmail).
pub const GMAIL_SCOPE: &str = "https://www.googleapis.com/auth/gmail.readonly";
