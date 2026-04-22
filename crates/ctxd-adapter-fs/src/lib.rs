//! Filesystem watcher adapter for ctxd.
//!
//! Watches a configured directory for file changes and publishes events
//! under `/work/local/files/{relative_path}`. Supports create, modify,
//! and delete events. Only text files (valid UTF-8) are ingested, and
//! file content is capped at 100KB to avoid bloating the event log.

mod watcher;

pub use watcher::FsAdapter;

/// Maximum file content size to include in events (100KB).
pub const MAX_CONTENT_SIZE: u64 = 100 * 1024;
