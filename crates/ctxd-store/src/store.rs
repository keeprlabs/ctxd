//! Core event store implementation backed by SQLite.

use ctxd_core::event::Event;
use ctxd_core::hash::PredecessorHash;
use ctxd_core::subject::Subject;
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use std::path::Path;

/// Errors from the event store.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// SQLite error.
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    /// JSON serialization error.
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// Hash chain integrity violation.
    #[error("hash chain violation: expected predecessor hash {expected}, got {actual}")]
    HashChainViolation {
        /// The expected predecessor hash.
        expected: String,
        /// The actual predecessor hash.
        actual: String,
    },

    /// Subject path error.
    #[error("subject error: {0}")]
    Subject(#[from] ctxd_core::subject::SubjectError),
}

/// The main event store. Owns a SQLite connection pool and provides
/// append/read operations with hash chain verification.
#[derive(Clone)]
pub struct EventStore {
    pool: SqlitePool,
}

impl EventStore {
    /// Open or create an event store at the given path.
    pub async fn open(path: &Path) -> Result<Self, StoreError> {
        let url = format!("sqlite:{}?mode=rwc", path.display());
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect(&url)
            .await?;
        let store = Self { pool };
        store.initialize().await?;
        Ok(store)
    }

    /// Open an in-memory event store (for testing).
    pub async fn open_memory() -> Result<Self, StoreError> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        let store = Self { pool };
        store.initialize().await?;
        Ok(store)
    }

    /// Create tables and indexes.
    async fn initialize(&self) -> Result<(), StoreError> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS events (
                seq INTEGER PRIMARY KEY AUTOINCREMENT,
                id TEXT NOT NULL UNIQUE,
                source TEXT NOT NULL,
                subject TEXT NOT NULL,
                event_type TEXT NOT NULL,
                time TEXT NOT NULL,
                datacontenttype TEXT NOT NULL DEFAULT 'application/json',
                data TEXT NOT NULL,
                predecessorhash TEXT,
                signature TEXT,
                specversion TEXT NOT NULL DEFAULT '1.0'
            );

            CREATE INDEX IF NOT EXISTS idx_events_subject ON events(subject);
            CREATE INDEX IF NOT EXISTS idx_events_time ON events(time);
            CREATE INDEX IF NOT EXISTS idx_events_type ON events(event_type);
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Metadata table for daemon config (e.g., root capability key)
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS metadata (
                key TEXT PRIMARY KEY,
                value BLOB NOT NULL
            );
            "#,
        )
        .execute(&self.pool)
        .await?;

        // KV view: latest value per subject
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS kv_view (
                subject TEXT PRIMARY KEY,
                event_id TEXT NOT NULL,
                data TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            "#,
        )
        .execute(&self.pool)
        .await?;

        // FTS5 view on event data
        sqlx::query(
            r#"
            CREATE VIRTUAL TABLE IF NOT EXISTS fts_view USING fts5(
                event_id,
                subject,
                event_type,
                data,
                content='events',
                content_rowid='seq'
            );
            "#,
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Append an event to the log.
    ///
    /// Computes and sets the predecessor hash based on the last event
    /// for the same subject tree. Updates materialized views.
    #[tracing::instrument(skip(self, event), fields(subject = %event.subject))]
    pub async fn append(&self, mut event: Event) -> Result<Event, StoreError> {
        // Compute predecessor hash from the last event in this subject's chain
        let last_event = self.last_event_for_subject(event.subject.as_str()).await?;
        if let Some(ref prev) = last_event {
            let hash = PredecessorHash::compute(prev).map_err(StoreError::Serialization)?;
            event.predecessorhash = Some(hash.to_string());
        }

        let id = event.id.to_string();
        let subject = event.subject.as_str().to_string();
        let data = serde_json::to_string(&event.data)?;
        let time = event.time.to_rfc3339();

        let mut tx = self.pool.begin().await?;

        let result = sqlx::query(
            r#"
            INSERT INTO events (id, source, subject, event_type, time, datacontenttype, data, predecessorhash, signature, specversion)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(&id)
        .bind(&event.source)
        .bind(&subject)
        .bind(&event.event_type)
        .bind(&time)
        .bind(&event.datacontenttype)
        .bind(&data)
        .bind(&event.predecessorhash)
        .bind(&event.signature)
        .bind(&event.specversion)
        .execute(&mut *tx)
        .await?;

        let seq = result.last_insert_rowid();

        // Update KV view
        sqlx::query(
            r#"
            INSERT INTO kv_view (subject, event_id, data, updated_at)
            VALUES (?, ?, ?, ?)
            ON CONFLICT(subject) DO UPDATE SET
                event_id = excluded.event_id,
                data = excluded.data,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(&subject)
        .bind(&id)
        .bind(&data)
        .bind(&time)
        .execute(&mut *tx)
        .await?;

        // Update FTS view
        sqlx::query(
            r#"
            INSERT INTO fts_view (rowid, event_id, subject, event_type, data)
            VALUES (?, ?, ?, ?, ?)
            "#,
        )
        .bind(seq)
        .bind(&id)
        .bind(&subject)
        .bind(&event.event_type)
        .bind(&data)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;

        Ok(event)
    }

    /// Read events for a subject, optionally recursive.
    #[tracing::instrument(skip(self), fields(subject = %subject))]
    pub async fn read(&self, subject: &Subject, recursive: bool) -> Result<Vec<Event>, StoreError> {
        let rows = if recursive {
            let pattern = if subject.as_str() == "/" {
                "/%".to_string()
            } else {
                format!("{}/%", subject.as_str())
            };
            sqlx::query_as::<_, EventRow>(
                "SELECT id, source, subject, event_type, time, datacontenttype, data, predecessorhash, signature, specversion FROM events WHERE subject = ? OR subject LIKE ? ORDER BY seq ASC",
            )
            .bind(subject.as_str())
            .bind(&pattern)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query_as::<_, EventRow>(
                "SELECT id, source, subject, event_type, time, datacontenttype, data, predecessorhash, signature, specversion FROM events WHERE subject = ? ORDER BY seq ASC",
            )
            .bind(subject.as_str())
            .fetch_all(&self.pool)
            .await?
        };

        rows.into_iter().map(|r| r.into_event()).collect()
    }

    /// List distinct subjects, optionally under a prefix.
    pub async fn subjects(
        &self,
        prefix: Option<&Subject>,
        recursive: bool,
    ) -> Result<Vec<String>, StoreError> {
        let rows: Vec<(String,)> = if let Some(pfx) = prefix {
            if recursive {
                let pattern = if pfx.as_str() == "/" {
                    "/%".to_string()
                } else {
                    format!("{}/%", pfx.as_str())
                };
                sqlx::query_as(
                    "SELECT DISTINCT subject FROM events WHERE subject = ? OR subject LIKE ? ORDER BY subject",
                )
                .bind(pfx.as_str())
                .bind(&pattern)
                .fetch_all(&self.pool)
                .await?
            } else {
                sqlx::query_as(
                    "SELECT DISTINCT subject FROM events WHERE subject = ? ORDER BY subject",
                )
                .bind(pfx.as_str())
                .fetch_all(&self.pool)
                .await?
            }
        } else {
            sqlx::query_as("SELECT DISTINCT subject FROM events ORDER BY subject")
                .fetch_all(&self.pool)
                .await?
        };

        Ok(rows.into_iter().map(|(s,)| s).collect())
    }

    /// Read events for a subject since a given timestamp, optionally recursive.
    pub async fn read_since(
        &self,
        subject: &Subject,
        since: chrono::DateTime<chrono::Utc>,
        recursive: bool,
    ) -> Result<Vec<Event>, StoreError> {
        let since_str = since.to_rfc3339();
        let rows = if recursive {
            let pattern = if subject.as_str() == "/" {
                "/%".to_string()
            } else {
                format!("{}/%", subject.as_str())
            };
            sqlx::query_as::<_, EventRow>(
                "SELECT id, source, subject, event_type, time, datacontenttype, data, predecessorhash, signature, specversion FROM events WHERE (subject = ? OR subject LIKE ?) AND time > ? ORDER BY seq ASC",
            )
            .bind(subject.as_str())
            .bind(&pattern)
            .bind(&since_str)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query_as::<_, EventRow>(
                "SELECT id, source, subject, event_type, time, datacontenttype, data, predecessorhash, signature, specversion FROM events WHERE subject = ? AND time > ? ORDER BY seq ASC",
            )
            .bind(subject.as_str())
            .bind(&since_str)
            .fetch_all(&self.pool)
            .await?
        };

        rows.into_iter().map(|r| r.into_event()).collect()
    }

    /// Search events using FTS5 full-text search.
    #[tracing::instrument(skip(self))]
    pub async fn search(
        &self,
        query: &str,
        limit: Option<usize>,
    ) -> Result<Vec<Event>, StoreError> {
        let sql = if limit.is_some() {
            r#"
            SELECT e.id, e.source, e.subject, e.event_type, e.time, e.datacontenttype, e.data, e.predecessorhash, e.signature, e.specversion
            FROM events e
            JOIN fts_view f ON e.seq = f.rowid
            WHERE fts_view MATCH ?
            ORDER BY e.seq ASC
            LIMIT ?
            "#
        } else {
            r#"
            SELECT e.id, e.source, e.subject, e.event_type, e.time, e.datacontenttype, e.data, e.predecessorhash, e.signature, e.specversion
            FROM events e
            JOIN fts_view f ON e.seq = f.rowid
            WHERE fts_view MATCH ?
            ORDER BY e.seq ASC
            "#
        };

        let rows: Vec<EventRow> = if let Some(lim) = limit {
            sqlx::query_as(sql)
                .bind(query)
                .bind(lim as i64)
                .fetch_all(&self.pool)
                .await?
        } else {
            sqlx::query_as(sql)
                .bind(query)
                .fetch_all(&self.pool)
                .await?
        };

        rows.into_iter().map(|r| r.into_event()).collect()
    }

    /// Get the latest value for a subject from the KV view.
    pub async fn kv_get(&self, subject: &str) -> Result<Option<serde_json::Value>, StoreError> {
        let row: Option<(String,)> = sqlx::query_as("SELECT data FROM kv_view WHERE subject = ?")
            .bind(subject)
            .fetch_optional(&self.pool)
            .await?;

        match row {
            Some((data,)) => Ok(Some(serde_json::from_str(&data)?)),
            None => Ok(None),
        }
    }

    /// Get a metadata value by key.
    pub async fn get_metadata(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
        let row: Option<(Vec<u8>,)> = sqlx::query_as("SELECT value FROM metadata WHERE key = ?")
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|(v,)| v))
    }

    /// Set a metadata value.
    pub async fn set_metadata(&self, key: &str, value: &[u8]) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO metadata (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(key)
        .bind(value)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Get the last event appended for a given subject path.
    async fn last_event_for_subject(&self, subject: &str) -> Result<Option<Event>, StoreError> {
        let row: Option<EventRow> = sqlx::query_as(
            "SELECT id, source, subject, event_type, time, datacontenttype, data, predecessorhash, signature, specversion FROM events WHERE subject = ? ORDER BY seq DESC LIMIT 1",
        )
        .bind(subject)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(r) => Ok(Some(r.into_event()?)),
            None => Ok(None),
        }
    }
}

/// Internal row type for SQLite query results.
#[derive(sqlx::FromRow)]
struct EventRow {
    id: String,
    source: String,
    subject: String,
    event_type: String,
    time: String,
    datacontenttype: String,
    data: String,
    predecessorhash: Option<String>,
    signature: Option<String>,
    specversion: String,
}

impl EventRow {
    fn into_event(self) -> Result<Event, StoreError> {
        Ok(Event {
            specversion: self.specversion,
            id: self.id.parse().map_err(|e| {
                StoreError::Database(sqlx::Error::Protocol(format!("bad uuid: {e}")))
            })?,
            source: self.source,
            subject: Subject::new(&self.subject)?,
            event_type: self.event_type,
            time: chrono::DateTime::parse_from_rfc3339(&self.time)
                .map_err(|e| StoreError::Database(sqlx::Error::Protocol(format!("bad time: {e}"))))?
                .with_timezone(&chrono::Utc),
            datacontenttype: self.datacontenttype,
            data: serde_json::from_str(&self.data)?,
            predecessorhash: self.predecessorhash,
            signature: self.signature,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn append_and_read() {
        let store = EventStore::open_memory().await.unwrap();
        let subject = Subject::new("/test/hello").unwrap();
        let event = Event::new(
            "ctxd://test".to_string(),
            subject.clone(),
            "demo".to_string(),
            serde_json::json!({"msg": "world"}),
        );

        let stored = store.append(event).await.unwrap();
        assert!(
            stored.predecessorhash.is_none(),
            "first event has no predecessor"
        );

        let events = store.read(&subject, false).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, serde_json::json!({"msg": "world"}));
    }

    #[tokio::test]
    async fn predecessor_hash_chain() {
        let store = EventStore::open_memory().await.unwrap();
        let subject = Subject::new("/test/chain").unwrap();

        let e1 = Event::new(
            "ctxd://test".to_string(),
            subject.clone(),
            "demo".to_string(),
            serde_json::json!({"step": 1}),
        );
        let stored1 = store.append(e1).await.unwrap();
        assert!(stored1.predecessorhash.is_none());

        let e2 = Event::new(
            "ctxd://test".to_string(),
            subject.clone(),
            "demo".to_string(),
            serde_json::json!({"step": 2}),
        );
        let stored2 = store.append(e2).await.unwrap();
        assert!(stored2.predecessorhash.is_some());

        // Verify the chain: hash of e1 should match e2's predecessor
        let expected = PredecessorHash::compute(&stored1).unwrap();
        assert_eq!(stored2.predecessorhash.as_ref().unwrap(), expected.as_str());
    }

    #[tokio::test]
    async fn recursive_read() {
        let store = EventStore::open_memory().await.unwrap();

        let subjects = vec!["/test", "/test/a", "/test/b", "/other"];
        for s in &subjects {
            let event = Event::new(
                "ctxd://test".to_string(),
                Subject::new(s).unwrap(),
                "demo".to_string(),
                serde_json::json!({"subject": s}),
            );
            store.append(event).await.unwrap();
        }

        let parent = Subject::new("/test").unwrap();
        let events = store.read(&parent, true).await.unwrap();
        assert_eq!(events.len(), 3); // /test, /test/a, /test/b

        let events = store.read(&parent, false).await.unwrap();
        assert_eq!(events.len(), 1); // only /test
    }

    #[tokio::test]
    async fn kv_view_latest_value() {
        let store = EventStore::open_memory().await.unwrap();
        let subject = Subject::new("/test/kv").unwrap();

        let e1 = Event::new(
            "ctxd://test".to_string(),
            subject.clone(),
            "demo".to_string(),
            serde_json::json!({"version": 1}),
        );
        store.append(e1).await.unwrap();

        let e2 = Event::new(
            "ctxd://test".to_string(),
            subject.clone(),
            "demo".to_string(),
            serde_json::json!({"version": 2}),
        );
        store.append(e2).await.unwrap();

        let latest = store.kv_get("/test/kv").await.unwrap().unwrap();
        assert_eq!(latest, serde_json::json!({"version": 2}));
    }

    #[tokio::test]
    async fn list_subjects() {
        let store = EventStore::open_memory().await.unwrap();

        for s in &["/a", "/a/b", "/a/c", "/d"] {
            let event = Event::new(
                "ctxd://test".to_string(),
                Subject::new(s).unwrap(),
                "demo".to_string(),
                serde_json::json!({}),
            );
            store.append(event).await.unwrap();
        }

        let all = store.subjects(None, false).await.unwrap();
        assert_eq!(all.len(), 4);

        let under_a = store
            .subjects(Some(&Subject::new("/a").unwrap()), true)
            .await
            .unwrap();
        assert_eq!(under_a.len(), 3); // /a, /a/b, /a/c
    }

    #[tokio::test]
    async fn fts_search() {
        let store = EventStore::open_memory().await.unwrap();

        let e1 = Event::new(
            "ctxd://test".to_string(),
            Subject::new("/docs/readme").unwrap(),
            "document".to_string(),
            serde_json::json!({"content": "hello world this is a test document"}),
        );
        store.append(e1).await.unwrap();

        let e2 = Event::new(
            "ctxd://test".to_string(),
            Subject::new("/docs/other").unwrap(),
            "document".to_string(),
            serde_json::json!({"content": "completely unrelated content here"}),
        );
        store.append(e2).await.unwrap();

        let results = store.search("hello world", None).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].subject.as_str(), "/docs/readme");
    }
}
