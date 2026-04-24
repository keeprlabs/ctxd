//! Gmail adapter for ctxd.
//!
//! Implements OAuth2 device-code authorization, AES-256-GCM at-rest token
//! encryption, and incremental Gmail History API polling. Publishes one
//! event per (message, label) pair into ctxd under
//! `/work/email/gmail/{label}/{message_id}`.
//!
//! # Subcommands
//! - `auth` — runs the OAuth2 device-code flow and persists an encrypted
//!   refresh token.
//! - `run` — loads the encrypted token, refreshes the access token, and
//!   syncs the inbox via the History API.
//! - `status` — prints the current sync state.
//!
//! # Modules
//! - [`oauth`] — OAuth2 device-code flow + token refresh.
//! - [`crypto`] — AES-256-GCM token encryption at rest.
//! - [`gmail`] — Gmail REST API client (messages.list, messages.get,
//!   history.list).
//! - [`parse`] — header parsing, body extraction, subject normalization.
//! - [`state`] — persisted sync cursor + idempotency tracking via SQLite.
//! - [`adapter`] — the [`GmailAdapter`] type that implements
//!   [`ctxd_adapter_core::Adapter`].

pub mod adapter;
pub mod crypto;
pub mod gmail;
pub mod oauth;
pub mod parse;
pub mod state;

pub use adapter::{GmailAdapter, GmailAdapterConfig};

/// Maximum body size we will store on an event, in bytes (128 KB).
pub const MAX_BODY_SIZE: usize = 128 * 1024;

/// Default labels to sync if the operator does not specify any.
pub const DEFAULT_LABELS: &[&str] = &["INBOX", "SENT"];

/// Default polling interval between history syncs.
pub const DEFAULT_POLL_INTERVAL_SECS: u64 = 60;

/// Default concurrency for parallel `messages.get` fetches.
pub const DEFAULT_FETCH_CONCURRENCY: usize = 10;

/// OAuth2 scope required for the adapter (read-only Gmail).
pub const GMAIL_SCOPE: &str = "https://www.googleapis.com/auth/gmail.readonly";
