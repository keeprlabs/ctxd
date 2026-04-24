//! Persistent state for the GitHub adapter.
//!
//! Stores three things in a single SQLite file:
//!
//! 1. `etags(url, etag)` — last `ETag` we received for a given fetch URL,
//!    used for `If-None-Match` revalidation.
//! 2. `cursors(scope, kind, since)` — last `updated_at` we successfully
//!    polled per (scope, kind); used to set the `since` query param so
//!    repeat polls don't re-walk the whole history.
//! 3. `seen_resources(kind, resource_key, last_updated_at, last_state)` —
//!    so we know if a resource is brand-new (publish `*.opened`) vs.
//!    something we've already seen (publish `*.updated`).
//!
//! All operations are async via sqlx. The DB is a per-host file at
//! `<state-dir>/github.state.db` and is created on first run.

use std::path::Path;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use sqlx::Row;

/// Errors that can occur opening or using the state DB.
#[derive(Debug, thiserror::Error)]
pub enum StateError {
    /// An sqlx error occurred.
    #[error("state DB error: {0}")]
    Sqlx(#[from] sqlx::Error),

    /// An I/O error occurred (creating state dir).
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Persistent adapter state.
pub struct StateDb {
    pool: SqlitePool,
}

impl StateDb {
    /// Open (or create) the state DB at `<dir>/github.state.db`.
    pub async fn open(dir: &Path) -> Result<Self, StateError> {
        tokio::fs::create_dir_all(dir).await?;
        let path = dir.join("github.state.db");
        let opts = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(opts)
            .await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS etags (
                url   TEXT PRIMARY KEY,
                etag  TEXT NOT NULL,
                fetched_at TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS cursors (
                scope TEXT NOT NULL,
                kind  TEXT NOT NULL,
                since TEXT NOT NULL,
                PRIMARY KEY (scope, kind)
            )",
        )
        .execute(&pool)
        .await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS seen_resources (
                kind            TEXT NOT NULL,
                resource_key    TEXT NOT NULL,
                last_updated_at TEXT NOT NULL,
                last_state      TEXT,
                PRIMARY KEY (kind, resource_key)
            )",
        )
        .execute(&pool)
        .await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS poll_meta (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await?;

        Ok(Self { pool })
    }

    /// Record an ETag returned for `url`.
    pub async fn put_etag(&self, url: &str, etag: &str) -> Result<(), StateError> {
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO etags (url, etag, fetched_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(url) DO UPDATE SET etag = excluded.etag, fetched_at = excluded.fetched_at",
        )
        .bind(url)
        .bind(etag)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Look up a saved ETag for `url`.
    pub async fn get_etag(&self, url: &str) -> Result<Option<String>, StateError> {
        let row = sqlx::query("SELECT etag FROM etags WHERE url = ?1")
            .bind(url)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| r.get::<String, _>("etag")))
    }

    /// Save a polling cursor.
    pub async fn put_cursor(&self, scope: &str, kind: &str, since: &str) -> Result<(), StateError> {
        sqlx::query(
            "INSERT INTO cursors (scope, kind, since) VALUES (?1, ?2, ?3)
             ON CONFLICT(scope, kind) DO UPDATE SET since = excluded.since",
        )
        .bind(scope)
        .bind(kind)
        .bind(since)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Read a polling cursor.
    pub async fn get_cursor(&self, scope: &str, kind: &str) -> Result<Option<String>, StateError> {
        let row = sqlx::query("SELECT since FROM cursors WHERE scope = ?1 AND kind = ?2")
            .bind(scope)
            .bind(kind)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| r.get::<String, _>("since")))
    }

    /// Look up the last known state for a resource, returning
    /// `Some((updated_at, state))` if we've seen it before.
    pub async fn get_seen(
        &self,
        kind: &str,
        resource_key: &str,
    ) -> Result<Option<(String, Option<String>)>, StateError> {
        let row = sqlx::query(
            "SELECT last_updated_at, last_state FROM seen_resources
             WHERE kind = ?1 AND resource_key = ?2",
        )
        .bind(kind)
        .bind(resource_key)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| {
            let upd: String = r.get("last_updated_at");
            let st: Option<String> = r.try_get("last_state").ok();
            (upd, st)
        }))
    }

    /// Mark a resource as seen.
    pub async fn put_seen(
        &self,
        kind: &str,
        resource_key: &str,
        last_updated_at: &str,
        last_state: Option<&str>,
    ) -> Result<(), StateError> {
        sqlx::query(
            "INSERT INTO seen_resources (kind, resource_key, last_updated_at, last_state)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(kind, resource_key) DO UPDATE SET
                last_updated_at = excluded.last_updated_at,
                last_state      = excluded.last_state",
        )
        .bind(kind)
        .bind(resource_key)
        .bind(last_updated_at)
        .bind(last_state)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Set a freeform metadata key (used for last-poll-at + rate-limit info
    /// surfaced via `status`).
    pub async fn put_meta(&self, key: &str, value: &str) -> Result<(), StateError> {
        sqlx::query(
            "INSERT INTO poll_meta (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(key)
        .bind(value)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Read a metadata key.
    pub async fn get_meta(&self, key: &str) -> Result<Option<String>, StateError> {
        let row = sqlx::query("SELECT value FROM poll_meta WHERE key = ?1")
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| r.get::<String, _>("value")))
    }

    /// List all cursors (for `status` output).
    pub async fn list_cursors(&self) -> Result<Vec<(String, String, String)>, StateError> {
        let rows = sqlx::query("SELECT scope, kind, since FROM cursors ORDER BY scope, kind")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .into_iter()
            .map(|r| {
                (
                    r.get::<String, _>("scope"),
                    r.get::<String, _>("kind"),
                    r.get::<String, _>("since"),
                )
            })
            .collect())
    }

    /// Close the connection pool. Optional — dropping does the same thing.
    pub async fn close(&self) {
        self.pool.close().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn open_creates_db_and_tables() {
        let dir = tempdir().unwrap();
        let db = StateDb::open(dir.path()).await.unwrap();
        assert!(dir.path().join("github.state.db").exists());
        // Tables present? Try a write/read.
        db.put_etag("https://x/y", "\"abc\"").await.unwrap();
        let got = db.get_etag("https://x/y").await.unwrap();
        assert_eq!(got.as_deref(), Some("\"abc\""));
    }

    #[tokio::test]
    async fn cursor_round_trip() {
        let dir = tempdir().unwrap();
        let db = StateDb::open(dir.path()).await.unwrap();
        assert!(db.get_cursor("acme/web", "issues").await.unwrap().is_none());
        db.put_cursor("acme/web", "issues", "2026-04-01T00:00:00Z")
            .await
            .unwrap();
        assert_eq!(
            db.get_cursor("acme/web", "issues")
                .await
                .unwrap()
                .as_deref(),
            Some("2026-04-01T00:00:00Z")
        );
        // Update.
        db.put_cursor("acme/web", "issues", "2026-04-02T00:00:00Z")
            .await
            .unwrap();
        assert_eq!(
            db.get_cursor("acme/web", "issues")
                .await
                .unwrap()
                .as_deref(),
            Some("2026-04-02T00:00:00Z")
        );
    }

    #[tokio::test]
    async fn seen_round_trip() {
        let dir = tempdir().unwrap();
        let db = StateDb::open(dir.path()).await.unwrap();
        assert!(db.get_seen("issue", "acme/web/1").await.unwrap().is_none());
        db.put_seen("issue", "acme/web/1", "2026-04-01T00:00:00Z", Some("open"))
            .await
            .unwrap();
        let got = db.get_seen("issue", "acme/web/1").await.unwrap().unwrap();
        assert_eq!(got.0, "2026-04-01T00:00:00Z");
        assert_eq!(got.1.as_deref(), Some("open"));
    }
}
