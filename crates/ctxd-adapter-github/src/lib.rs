//! GitHub adapter for ctxd.
//!
//! Authenticates with a personal access token (PAT) and incrementally polls the
//! GitHub REST API for issues, pull requests, comments, and notifications,
//! publishing events into ctxd under `/work/github/...`.
//!
//! High-level structure:
//!
//! - [`client`] — thin reqwest wrapper that adds auth + version headers, parses
//!   rate-limit headers, follows pagination via the `Link` header, and serves
//!   `If-None-Match` from the persisted ETag store.
//! - [`state`] — sqlx-sqlite state DB that stores per-endpoint cursors
//!   (`since`) and ETags so polls are idempotent across restarts.
//! - [`events`] — pruning + truncation of GitHub JSON into the ctxd event
//!   payload, plus deterministic event-type derivation.
//! - [`parse`] — link header + retry-after parsing.
//! - [`poller`] — main polling loop that ties the pieces together.
//! - [`adapter`] — implements [`ctxd_adapter_core::Adapter`].

#![deny(missing_docs)]

pub mod adapter;
pub mod client;
pub mod config;
pub mod events;
pub mod parse;
pub mod poller;
pub mod state;

pub use adapter::GitHubAdapter;
pub use config::{Config, RepoSelector, ResourceKind};

/// Maximum body size (UTF-8 bytes) embedded in any single event.
///
/// Bodies larger than this are truncated with a single `…` byte sentinel
/// appended; the original size is still recorded under `body_full_size`.
pub const MAX_BODY_BYTES: usize = 16 * 1024;

/// User-Agent header sent on every request.
pub const USER_AGENT: &str = "ctxd-adapter-github/0.3";

/// GitHub REST API version header value.
pub const GITHUB_API_VERSION: &str = "2022-11-28";
