//! Persisted state for the Gmail adapter.
//!
//! Two pieces of state live here:
//!
//! 1. A cursor: the last `historyId` we've seen, plus the last poll
//!    timestamp.
//! 2. An idempotency table: `gmail_internal_id`s we've already
//!    published. Restart after a crash must not re-publish events.
//!
//! Both are stored in a small SQLite file at
//! `<state-dir>/gmail.state.db` via `sqlx`. We bring a connection up
//! lazily and pin it via [`StateStore::open`].

use std::path::Path;

use chrono::{DateTime, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use sqlx::ConnectOptions;

/// Errors produced by the state store.
#[derive(Debug, thiserror::Error)]
pub enum StateError {
    /// Underlying SQLite error.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] sqlx::Error),

    /// Filesystem error setting up the state-dir.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// Stored timestamp could not be parsed.
    #[error("invalid timestamp in state DB: {0}")]
    InvalidTimestamp(String),
}

/// Persisted Gmail sync state.
pub struct StateStore {
    pool: SqlitePool,
}

/// Snapshot of the current sync cursor.
#[derive(Debug, Clone)]
pub struct SyncCursor {
    /// Last `historyId` we successfully synced from (or `None` if no
    /// sync has run yet).
    pub history_id: Option<String>,
    /// When the last poll completed.
    pub last_poll_at: Option<DateTime<Utc>>,
}

impl StateStore {
    /// Open (or create) the state DB at the given path.
    pub async fn open(path: &Path) -> Result<Self, StateError> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .log_statements(tracing::log::LevelFilter::Trace);

        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(opts)
            .await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS sync_state (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                history_id TEXT,
                last_poll_at TEXT
            )",
        )
        .execute(&pool)
        .await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS published_messages (
                gmail_internal_id TEXT NOT NULL,
                label TEXT NOT NULL,
                published_at TEXT NOT NULL,
                PRIMARY KEY (gmail_internal_id, label)
            )",
        )
        .execute(&pool)
        .await?;

        Ok(Self { pool })
    }

    /// Read the current sync cursor.
    pub async fn cursor(&self) -> Result<SyncCursor, StateError> {
        let row: Option<(Option<String>, Option<String>)> =
            sqlx::query_as("SELECT history_id, last_poll_at FROM sync_state WHERE id = 1")
                .fetch_optional(&self.pool)
                .await?;
        match row {
            Some((hid, last)) => {
                let last_poll_at = match last {
                    Some(s) => Some(
                        DateTime::parse_from_rfc3339(&s)
                            .map_err(|e| StateError::InvalidTimestamp(e.to_string()))?
                            .with_timezone(&Utc),
                    ),
                    None => None,
                };
                Ok(SyncCursor {
                    history_id: hid,
                    last_poll_at,
                })
            }
            None => Ok(SyncCursor {
                history_id: None,
                last_poll_at: None,
            }),
        }
    }

    /// Persist a new cursor + poll timestamp.
    pub async fn set_cursor(
        &self,
        history_id: &str,
        last_poll_at: DateTime<Utc>,
    ) -> Result<(), StateError> {
        let ts = last_poll_at.to_rfc3339();
        sqlx::query(
            "INSERT INTO sync_state (id, history_id, last_poll_at) VALUES (1, ?, ?)
             ON CONFLICT(id) DO UPDATE SET history_id = excluded.history_id, last_poll_at = excluded.last_poll_at",
        )
        .bind(history_id)
        .bind(ts)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Check whether `(gmail_internal_id, label)` has already been
    /// published.
    pub async fn is_published(
        &self,
        gmail_internal_id: &str,
        label: &str,
    ) -> Result<bool, StateError> {
        let row: Option<(i64,)> = sqlx::query_as(
            "SELECT 1 FROM published_messages WHERE gmail_internal_id = ? AND label = ?",
        )
        .bind(gmail_internal_id)
        .bind(label)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.is_some())
    }

    /// Record a `(gmail_internal_id, label)` as published. Idempotent —
    /// re-recording the same row is a no-op.
    pub async fn mark_published(
        &self,
        gmail_internal_id: &str,
        label: &str,
    ) -> Result<(), StateError> {
        let ts = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO published_messages (gmail_internal_id, label, published_at) VALUES (?, ?, ?)
             ON CONFLICT(gmail_internal_id, label) DO NOTHING",
        )
        .bind(gmail_internal_id)
        .bind(label)
        .bind(ts)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Total number of `(message, label)` pairs we've published. Used
    /// by `status`.
    pub async fn published_count(&self) -> Result<i64, StateError> {
        let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM published_messages")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn fresh() -> (TempDir, StateStore) {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("gmail.state.db");
        let store = StateStore::open(&path).await.expect("open");
        (dir, store)
    }

    #[tokio::test]
    async fn cursor_round_trip() {
        let (_dir, store) = fresh().await;
        let cursor = store.cursor().await.unwrap();
        assert!(cursor.history_id.is_none());

        let now = Utc::now();
        store.set_cursor("12345", now).await.unwrap();

        let cursor = store.cursor().await.unwrap();
        assert_eq!(cursor.history_id.as_deref(), Some("12345"));
        assert!(cursor.last_poll_at.is_some());
    }

    #[tokio::test]
    async fn idempotent_publish_marker() {
        let (_dir, store) = fresh().await;

        assert!(!store.is_published("msg1", "inbox").await.unwrap());

        store.mark_published("msg1", "inbox").await.unwrap();
        assert!(store.is_published("msg1", "inbox").await.unwrap());

        // Re-marking is a no-op.
        store.mark_published("msg1", "inbox").await.unwrap();
        assert_eq!(store.published_count().await.unwrap(), 1);

        // Different label of same message is a different row.
        store.mark_published("msg1", "sent").await.unwrap();
        assert_eq!(store.published_count().await.unwrap(), 2);
    }
}
