//! Core event store implementation backed by SQLite.

use ctxd_core::event::Event;
use ctxd_core::hash::PredecessorHash;
use ctxd_core::signing::EventSigner;
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
#[derive(Clone, Debug)]
pub struct EventStore {
    pool: SqlitePool,
    /// Optional Ed25519 signing key for event signatures.
    signing_key: Option<Vec<u8>>,
}

impl EventStore {
    /// Open or create an event store at the given path.
    pub async fn open(path: &Path) -> Result<Self, StoreError> {
        let url = format!("sqlite:{}?mode=rwc", path.display());
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect(&url)
            .await?;
        let store = Self {
            pool,
            signing_key: None,
        };
        store.initialize().await?;
        Ok(store)
    }

    /// Open an in-memory event store (for testing).
    pub async fn open_memory() -> Result<Self, StoreError> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        let store = Self {
            pool,
            signing_key: None,
        };
        store.initialize().await?;
        Ok(store)
    }

    /// Create tables and indexes.
    ///
    /// All schema migrations are additive. v0.3 adds `parents` and
    /// `attestation` columns to `events` and introduces federation
    /// (`peers`, `peer_cursors`, `event_parents`), budgets, approvals,
    /// and vector embedding tables.
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
                specversion TEXT NOT NULL DEFAULT '1.0',
                parents TEXT,
                attestation BLOB
            );

            CREATE INDEX IF NOT EXISTS idx_events_subject ON events(subject);
            CREATE INDEX IF NOT EXISTS idx_events_time ON events(time);
            CREATE INDEX IF NOT EXISTS idx_events_type ON events(event_type);
            "#,
        )
        .execute(&self.pool)
        .await?;

        // v0.3 additive migrations for pre-v0.3 databases. SQLite ignores
        // duplicate ALTER TABLE on re-open because we swallow the error
        // if the column already exists.
        for alter in [
            "ALTER TABLE events ADD COLUMN parents TEXT",
            "ALTER TABLE events ADD COLUMN attestation BLOB",
        ] {
            if let Err(e) = sqlx::query(alter).execute(&self.pool).await {
                // SQLite reports "duplicate column name" if the column is
                // already there. Every other error is real.
                let msg = e.to_string();
                if !msg.contains("duplicate column name") {
                    return Err(e.into());
                }
            }
        }

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

        // Revoked tokens table
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS revoked_tokens (
                token_id TEXT PRIMARY KEY,
                revoked_at TEXT NOT NULL
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

        // Graph view: entities
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS graph_entities (
                id TEXT PRIMARY KEY,
                entity_type TEXT NOT NULL,
                name TEXT NOT NULL,
                properties TEXT NOT NULL DEFAULT '{}',
                source_event_id TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_entities_type ON graph_entities(entity_type);
            CREATE INDEX IF NOT EXISTS idx_entities_name ON graph_entities(name);
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Graph view: relationships
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS graph_relationships (
                id TEXT PRIMARY KEY,
                from_entity_id TEXT NOT NULL,
                to_entity_id TEXT NOT NULL,
                relationship_type TEXT NOT NULL,
                properties TEXT NOT NULL DEFAULT '{}',
                source_event_id TEXT NOT NULL,
                created_at TEXT NOT NULL,
                FOREIGN KEY (from_entity_id) REFERENCES graph_entities(id),
                FOREIGN KEY (to_entity_id) REFERENCES graph_entities(id)
            );
            CREATE INDEX IF NOT EXISTS idx_rel_from ON graph_relationships(from_entity_id);
            CREATE INDEX IF NOT EXISTS idx_rel_to ON graph_relationships(to_entity_id);
            CREATE INDEX IF NOT EXISTS idx_rel_type ON graph_relationships(relationship_type);
            "#,
        )
        .execute(&self.pool)
        .await?;

        // v0.3: event_parents. Normalized many-to-many side table for
        // concurrent-branch parents. The `parents` column on events is the
        // canonical form (sorted, comma-joined); this table exists so
        // queries like "which events have parent X?" can use an index.
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS event_parents (
                event_id TEXT NOT NULL,
                parent_id TEXT NOT NULL,
                PRIMARY KEY (event_id, parent_id)
            );
            CREATE INDEX IF NOT EXISTS idx_event_parents_parent ON event_parents(parent_id);
            "#,
        )
        .execute(&self.pool)
        .await?;

        // v0.3: peers for federation. `public_key` is 32 raw Ed25519 bytes.
        // `granted_subjects` is a JSON array of glob patterns this peer may
        // receive. `trust_level` is a free-form JSON for future auto-accept
        // policy.
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS peers (
                peer_id TEXT PRIMARY KEY,
                url TEXT NOT NULL,
                public_key BLOB NOT NULL,
                granted_subjects TEXT NOT NULL DEFAULT '[]',
                trust_level TEXT NOT NULL DEFAULT '{}',
                added_at TEXT NOT NULL
            );
            "#,
        )
        .execute(&self.pool)
        .await?;

        // v0.3: peer_cursors for federation resume. Per-peer per-subject
        // pattern cursor tracking the last event time/id we have exchanged.
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS peer_cursors (
                peer_id TEXT NOT NULL,
                subject_pattern TEXT NOT NULL,
                last_event_id TEXT,
                last_event_time TEXT,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (peer_id, subject_pattern)
            );
            "#,
        )
        .execute(&self.pool)
        .await?;

        // v0.3: token_budgets for BudgetLimit caveat. `currency` is a free
        // ISO-style string (e.g. "USD_micro"). `spent` is monotonically
        // increasing micro-units.
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS token_budgets (
                token_id TEXT NOT NULL,
                currency TEXT NOT NULL,
                spent INTEGER NOT NULL DEFAULT 0,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (token_id, currency)
            );
            "#,
        )
        .execute(&self.pool)
        .await?;

        // v0.3: pending_approvals for HumanApprovalRequired caveat.
        // Decisions: 'pending', 'allow', 'deny'.
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS pending_approvals (
                approval_id TEXT PRIMARY KEY,
                token_id TEXT NOT NULL,
                operation TEXT NOT NULL,
                subject TEXT NOT NULL,
                decision TEXT NOT NULL DEFAULT 'pending',
                requested_at TEXT NOT NULL,
                decided_at TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_approvals_decision ON pending_approvals(decision);
            "#,
        )
        .execute(&self.pool)
        .await?;

        // v0.3: vector_embeddings. Raw vectors stored so a persisted HNSW
        // index can be rebuilt on startup from SQLite alone.
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS vector_embeddings (
                event_id TEXT PRIMARY KEY,
                model TEXT NOT NULL,
                dimensions INTEGER NOT NULL,
                vector BLOB NOT NULL,
                created_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_vectors_model ON vector_embeddings(model);
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
    ///
    /// Federation note: if the event arrives with a non-`None`
    /// `predecessorhash` we treat it as authoritative (the originating
    /// peer computed it) and skip recomputation. The signature over the
    /// canonical form binds the predecessorhash, so any tampering breaks
    /// signature verification at the inbound checkpoint.
    #[tracing::instrument(skip(self, event), fields(subject = %event.subject))]
    pub async fn append(&self, mut event: Event) -> Result<Event, StoreError> {
        // Compute predecessor hash from the last event in this subject's
        // chain — but only if the caller hasn't supplied one. Replicated
        // events come with their origin's predecessorhash baked into
        // their signature; recomputing would invalidate the signature.
        if event.predecessorhash.is_none() {
            let last_event = self.last_event_for_subject(event.subject.as_str()).await?;
            if let Some(ref prev) = last_event {
                let hash = PredecessorHash::compute(prev).map_err(StoreError::Serialization)?;
                event.predecessorhash = Some(hash.to_string());
            }
        }

        // Sign the event if a signing key is available AND the event
        // is not already signed. Replicated events arrive pre-signed by
        // their origin peer; we must NOT re-sign with the local key
        // because that would invalidate cross-peer signature
        // verification. Signing failures (malformed key bytes or
        // serialization errors) are logged via tracing and the event
        // is appended unsigned — we do not want a misconfigured key to
        // block writes.
        if event.signature.is_none() {
            if let Some(ref key) = self.signing_key {
                match EventSigner::from_bytes(key) {
                    Ok(signer) => match signer.sign(&event) {
                        Ok(sig) => event.signature = Some(sig),
                        Err(e) => {
                            tracing::warn!(error = %e, "failed to sign event; appending unsigned");
                        }
                    },
                    Err(e) => {
                        tracing::warn!(error = %e, "invalid signing key bytes; appending unsigned");
                    }
                }
            }
        }

        let id = event.id.to_string();
        let subject = event.subject.as_str().to_string();
        let data = serde_json::to_string(&event.data)?;
        let time = event.time.to_rfc3339();

        // Canonical parents column: sorted UUIDs joined by ",". Empty string
        // is represented as NULL so a pre-v0.3 column (all-NULL) and a
        // freshly-written v0.3 event with no parents look the same.
        let parents_sorted = event.parents_sorted();
        let parents_col: Option<String> = if parents_sorted.is_empty() {
            None
        } else {
            Some(
                parents_sorted
                    .iter()
                    .map(|u| u.to_string())
                    .collect::<Vec<_>>()
                    .join(","),
            )
        };

        let mut tx = self.pool.begin().await?;

        let result = sqlx::query(
            r#"
            INSERT INTO events (id, source, subject, event_type, time, datacontenttype, data, predecessorhash, signature, specversion, parents, attestation)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
        .bind(&parents_col)
        .bind(event.attestation.as_deref())
        .execute(&mut *tx)
        .await?;

        let seq = result.last_insert_rowid();

        // Side-table for efficient parent lookups.
        for parent in &parents_sorted {
            sqlx::query("INSERT OR IGNORE INTO event_parents (event_id, parent_id) VALUES (?, ?)")
                .bind(&id)
                .bind(parent.to_string())
                .execute(&mut *tx)
                .await?;
        }

        // Update KV view under the federation LWW rule (ADR 006):
        // overwrite the row only when the incoming event has a strictly
        // greater `(time, event_id)` tuple. UUIDv7 ids are
        // lexicographically monotonic in their embedded ms timestamp,
        // so the same SQL works as a deterministic tiebreak.
        sqlx::query(
            r#"
            INSERT INTO kv_view (subject, event_id, data, updated_at)
            VALUES (?, ?, ?, ?)
            ON CONFLICT(subject) DO UPDATE SET
                event_id = excluded.event_id,
                data = excluded.data,
                updated_at = excluded.updated_at
            WHERE (excluded.updated_at, excluded.event_id) > (kv_view.updated_at, kv_view.event_id)
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
                "SELECT id, source, subject, event_type, time, datacontenttype, data, predecessorhash, signature, specversion, parents, attestation FROM events WHERE subject = ? OR subject LIKE ? ORDER BY seq ASC",
            )
            .bind(subject.as_str())
            .bind(&pattern)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query_as::<_, EventRow>(
                "SELECT id, source, subject, event_type, time, datacontenttype, data, predecessorhash, signature, specversion, parents, attestation FROM events WHERE subject = ? ORDER BY seq ASC",
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
                "SELECT id, source, subject, event_type, time, datacontenttype, data, predecessorhash, signature, specversion, parents, attestation FROM events WHERE (subject = ? OR subject LIKE ?) AND time > ? ORDER BY seq ASC",
            )
            .bind(subject.as_str())
            .bind(&pattern)
            .bind(&since_str)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query_as::<_, EventRow>(
                "SELECT id, source, subject, event_type, time, datacontenttype, data, predecessorhash, signature, specversion, parents, attestation FROM events WHERE subject = ? AND time > ? ORDER BY seq ASC",
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
            SELECT e.id, e.source, e.subject, e.event_type, e.time, e.datacontenttype, e.data, e.predecessorhash, e.signature, e.specversion, e.parents, e.attestation
            FROM events e
            JOIN fts_view f ON e.seq = f.rowid
            WHERE fts_view MATCH ?
            ORDER BY e.seq ASC
            LIMIT ?
            "#
        } else {
            r#"
            SELECT e.id, e.source, e.subject, e.event_type, e.time, e.datacontenttype, e.data, e.predecessorhash, e.signature, e.specversion, e.parents, e.attestation
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

    /// Set the Ed25519 signing key for event signatures.
    pub fn set_signing_key(&mut self, key: Vec<u8>) {
        self.signing_key = Some(key);
    }

    /// Revoke a token by its token_id.
    pub async fn revoke_token(&self, token_id: &str) -> Result<(), StoreError> {
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO revoked_tokens (token_id, revoked_at) VALUES (?, ?) ON CONFLICT(token_id) DO NOTHING",
        )
        .bind(token_id)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Check if a token has been revoked.
    pub async fn is_token_revoked(&self, token_id: &str) -> Result<bool, StoreError> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT token_id FROM revoked_tokens WHERE token_id = ?")
                .bind(token_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.is_some())
    }

    /// Create a `GraphView` backed by the same connection pool.
    pub fn graph_view(&self) -> crate::views::graph::GraphView {
        crate::views::graph::GraphView::new(self.pool.clone())
    }

    /// Access the underlying connection pool. Exposed so that the
    /// `Store` trait impl in [`crate::store_trait`] can perform extra
    /// queries without re-opening a connection.
    #[doc(hidden)]
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Get the state of a subject at a specific point in time.
    /// Returns events for this subject with time <= as_of, ordered by seq ASC.
    pub async fn read_at(
        &self,
        subject: &Subject,
        as_of: chrono::DateTime<chrono::Utc>,
        recursive: bool,
    ) -> Result<Vec<Event>, StoreError> {
        let as_of_str = as_of.to_rfc3339();
        let rows = if recursive {
            let pattern = if subject.as_str() == "/" {
                "/%".to_string()
            } else {
                format!("{}/%", subject.as_str())
            };
            sqlx::query_as::<_, EventRow>(
                "SELECT id, source, subject, event_type, time, datacontenttype, data, predecessorhash, signature, specversion, parents, attestation FROM events WHERE (subject = ? OR subject LIKE ?) AND time <= ? ORDER BY seq ASC",
            )
            .bind(subject.as_str())
            .bind(&pattern)
            .bind(&as_of_str)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query_as::<_, EventRow>(
                "SELECT id, source, subject, event_type, time, datacontenttype, data, predecessorhash, signature, specversion, parents, attestation FROM events WHERE subject = ? AND time <= ? ORDER BY seq ASC",
            )
            .bind(subject.as_str())
            .bind(&as_of_str)
            .fetch_all(&self.pool)
            .await?
        };

        rows.into_iter().map(|r| r.into_event()).collect()
    }

    /// Get the KV view state at a point in time.
    /// Returns the latest event data for this subject with time <= as_of.
    pub async fn kv_get_at(
        &self,
        subject: &str,
        as_of: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<serde_json::Value>, StoreError> {
        let as_of_str = as_of.to_rfc3339();
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT data FROM events WHERE subject = ? AND time <= ? ORDER BY seq DESC LIMIT 1",
        )
        .bind(subject)
        .bind(&as_of_str)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some((data,)) => Ok(Some(serde_json::from_str(&data)?)),
            None => Ok(None),
        }
    }

    /// Get the last event appended for a given subject path.
    async fn last_event_for_subject(&self, subject: &str) -> Result<Option<Event>, StoreError> {
        let row: Option<EventRow> = sqlx::query_as(
            "SELECT id, source, subject, event_type, time, datacontenttype, data, predecessorhash, signature, specversion, parents, attestation FROM events WHERE subject = ? ORDER BY seq DESC LIMIT 1",
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
    /// v0.3: comma-separated UUIDs. `None` or empty string means no parents.
    parents: Option<String>,
    /// v0.3: raw attestation bytes.
    attestation: Option<Vec<u8>>,
}

impl EventRow {
    fn into_event(self) -> Result<Event, StoreError> {
        let parents: Vec<uuid::Uuid> = match self.parents.as_deref() {
            None | Some("") => Vec::new(),
            Some(s) => s
                .split(',')
                .map(|p| {
                    p.parse::<uuid::Uuid>().map_err(|e| {
                        StoreError::Database(sqlx::Error::Protocol(format!("bad parent uuid: {e}")))
                    })
                })
                .collect::<Result<_, _>>()?,
        };
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
            parents,
            attestation: self.attestation,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn append_with_parents_and_attestation_roundtrips() {
        let store = EventStore::open_memory().await.unwrap();
        let subject = Subject::new("/merge/test").unwrap();
        let p1 = uuid::Uuid::parse_str("00000000-0000-7000-8000-000000000001").unwrap();
        let p2 = uuid::Uuid::parse_str("00000000-0000-7000-8000-000000000002").unwrap();

        let mut event = Event::new(
            "ctxd://test".to_string(),
            subject.clone(),
            "demo".to_string(),
            serde_json::json!({"step": "merge"}),
        );
        event.parents = vec![p2, p1]; // insertion order differs from sort order
        event.attestation = Some(vec![0xba, 0xad, 0xf0, 0x0d]);

        let stored = store.append(event).await.unwrap();
        // Round-trip: read back and confirm fields survived.
        let events = store.read(&subject, false).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, stored.id);
        // parents should come back sorted on read (DB stores sorted form).
        assert_eq!(events[0].parents, vec![p1, p2]);
        assert_eq!(
            events[0].attestation.as_deref(),
            Some(&[0xba, 0xad, 0xf0, 0x0du8][..])
        );

        // The event_parents side table should have two rows for this event.
        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM event_parents WHERE event_id = ?")
            .bind(stored.id.to_string())
            .fetch_one(&store.pool)
            .await
            .unwrap();
        assert_eq!(count.0, 2);
    }

    #[tokio::test]
    async fn v03_schema_tables_exist() {
        let store = EventStore::open_memory().await.unwrap();
        // All v0.3 tables should exist and be queryable.
        for table in [
            "event_parents",
            "peers",
            "peer_cursors",
            "token_budgets",
            "pending_approvals",
            "vector_embeddings",
        ] {
            let row: (i64,) = sqlx::query_as(&format!("SELECT COUNT(*) FROM {table}"))
                .fetch_one(&store.pool)
                .await
                .unwrap_or_else(|e| panic!("{table} missing: {e}"));
            assert_eq!(row.0, 0, "{table} should start empty");
        }
    }

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

    #[tokio::test]
    async fn concurrent_appends_10_tasks_100_events_each() {
        let store = EventStore::open_memory().await.unwrap();
        let store = std::sync::Arc::new(store);

        let mut handles = Vec::new();
        for task_id in 0..10 {
            let store = std::sync::Arc::clone(&store);
            handles.push(tokio::spawn(async move {
                for i in 0..100 {
                    let subject = Subject::new(&format!("/concurrent/task{task_id}")).unwrap();
                    let event = Event::new(
                        "ctxd://test".to_string(),
                        subject,
                        "demo".to_string(),
                        serde_json::json!({"task": task_id, "seq": i}),
                    );
                    store.append(event).await.unwrap();
                }
            }));
        }

        for handle in handles {
            handle.await.unwrap();
        }

        // Verify all 1000 events are present
        let root = Subject::new("/concurrent").unwrap();
        let all_events = store.read(&root, true).await.unwrap();
        assert_eq!(all_events.len(), 1000);

        // Verify each task has exactly 100 events
        for task_id in 0..10 {
            let subject = Subject::new(&format!("/concurrent/task{task_id}")).unwrap();
            let events = store.read(&subject, false).await.unwrap();
            assert_eq!(events.len(), 100, "task {task_id} should have 100 events");

            // Verify hash chain integrity for this subject
            for i in 1..events.len() {
                assert!(
                    events[i].predecessorhash.is_some(),
                    "event {i} in task {task_id} should have predecessor hash"
                );
                let expected = PredecessorHash::compute(&events[i - 1]).unwrap();
                assert_eq!(
                    events[i].predecessorhash.as_ref().unwrap(),
                    expected.as_str(),
                    "hash chain broken at event {i} in task {task_id}"
                );
            }
        }
    }

    #[tokio::test]
    async fn large_dataset_10000_events() {
        let store = EventStore::open_memory().await.unwrap();

        for i in 0..10_000 {
            let bucket = i % 100;
            let subject = Subject::new(&format!("/large/bucket{bucket}")).unwrap();
            let event = Event::new(
                "ctxd://test".to_string(),
                subject,
                "demo".to_string(),
                serde_json::json!({"index": i}),
            );
            store.append(event).await.unwrap();
        }

        // Verify recursive read returns all events
        let root = Subject::new("/large").unwrap();
        let all_events = store.read(&root, true).await.unwrap();
        assert_eq!(all_events.len(), 10_000);

        // Verify FTS search works over large dataset
        let results = store.search("index", None).await.unwrap();
        assert_eq!(results.len(), 10_000);

        // Verify search with limit
        let results = store.search("index", Some(10)).await.unwrap();
        assert_eq!(results.len(), 10);
    }

    #[tokio::test]
    async fn kv_view_consistency_100_writes() {
        let store = EventStore::open_memory().await.unwrap();
        let subject = Subject::new("/kv/consistency").unwrap();

        for i in 0..100 {
            let event = Event::new(
                "ctxd://test".to_string(),
                subject.clone(),
                "demo".to_string(),
                serde_json::json!({"version": i}),
            );
            store.append(event).await.unwrap();
        }

        // KV should return the very last value
        let latest = store.kv_get("/kv/consistency").await.unwrap().unwrap();
        assert_eq!(latest, serde_json::json!({"version": 99}));

        // Verify all 100 events are in the log
        let events = store.read(&subject, false).await.unwrap();
        assert_eq!(events.len(), 100);
    }

    #[tokio::test]
    async fn search_relevance_ordering() {
        let store = EventStore::open_memory().await.unwrap();

        let e1 = Event::new(
            "ctxd://test".to_string(),
            Subject::new("/search/first").unwrap(),
            "document".to_string(),
            serde_json::json!({"content": "the quick brown fox jumps over the lazy dog"}),
        );
        store.append(e1).await.unwrap();

        let e2 = Event::new(
            "ctxd://test".to_string(),
            Subject::new("/search/second").unwrap(),
            "document".to_string(),
            serde_json::json!({"content": "a completely different topic about databases"}),
        );
        store.append(e2).await.unwrap();

        let e3 = Event::new(
            "ctxd://test".to_string(),
            Subject::new("/search/third").unwrap(),
            "document".to_string(),
            serde_json::json!({"content": "the fox was quick and brown"}),
        );
        store.append(e3).await.unwrap();

        // Search for "fox" should return the events that contain it
        let results = store.search("fox", None).await.unwrap();
        assert_eq!(results.len(), 2);
        let subjects: Vec<&str> = results.iter().map(|e| e.subject.as_str()).collect();
        assert!(subjects.contains(&"/search/first"));
        assert!(subjects.contains(&"/search/third"));
        assert!(!subjects.contains(&"/search/second"));
    }

    #[tokio::test]
    async fn kv_view_and_events_always_agree() {
        let store = EventStore::open_memory().await.unwrap();

        for i in 0..50 {
            let subject = Subject::new(&format!("/txn/subj{}", i % 5)).unwrap();
            let event = Event::new(
                "ctxd://test".to_string(),
                subject,
                "demo".to_string(),
                serde_json::json!({"val": i}),
            );
            store.append(event).await.unwrap();
        }

        // For each subject, the KV view data should match the last event's data
        for j in 0..5 {
            let subject_str = format!("/txn/subj{j}");
            let subject = Subject::new(&subject_str).unwrap();
            let events = store.read(&subject, false).await.unwrap();
            let last_event = events.last().unwrap();
            let kv_data = store.kv_get(&subject_str).await.unwrap().unwrap();
            assert_eq!(
                last_event.data, kv_data,
                "KV view disagrees with event log for {subject_str}"
            );
        }
    }

    #[tokio::test]
    async fn kv_get_nonexistent_returns_none() {
        let store = EventStore::open_memory().await.unwrap();
        let result = store.kv_get("/does/not/exist").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn read_at_returns_events_up_to_timestamp() {
        use chrono::TimeZone;
        let store = EventStore::open_memory().await.unwrap();
        let subject = Subject::new("/temporal/test").unwrap();

        let t1 = chrono::Utc.with_ymd_and_hms(2025, 1, 1, 10, 0, 0).unwrap();
        let t2 = chrono::Utc.with_ymd_and_hms(2025, 1, 1, 11, 0, 0).unwrap();
        let t3 = chrono::Utc.with_ymd_and_hms(2025, 1, 1, 12, 0, 0).unwrap();

        for (i, t) in [(1, t1), (2, t2), (3, t3)] {
            let mut event = Event::new(
                "ctxd://test".to_string(),
                subject.clone(),
                "demo".to_string(),
                serde_json::json!({"version": i}),
            );
            event.time = t;
            store.append(event).await.unwrap();
        }

        // Query at t2: should return events at t1 and t2 only
        let events = store.read_at(&subject, t2, false).await.unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].data, serde_json::json!({"version": 1}));
        assert_eq!(events[1].data, serde_json::json!({"version": 2}));

        // Query at t1: should return only the first event
        let events = store.read_at(&subject, t1, false).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, serde_json::json!({"version": 1}));

        // Query at t3: should return all three
        let events = store.read_at(&subject, t3, false).await.unwrap();
        assert_eq!(events.len(), 3);
    }

    #[tokio::test]
    async fn read_at_recursive() {
        use chrono::TimeZone;
        let store = EventStore::open_memory().await.unwrap();

        let t1 = chrono::Utc.with_ymd_and_hms(2025, 1, 1, 10, 0, 0).unwrap();
        let t2 = chrono::Utc.with_ymd_and_hms(2025, 1, 1, 11, 0, 0).unwrap();

        let mut e1 = Event::new(
            "ctxd://test".to_string(),
            Subject::new("/temporal/a").unwrap(),
            "demo".to_string(),
            serde_json::json!({"sub": "a"}),
        );
        e1.time = t1;
        store.append(e1).await.unwrap();

        let mut e2 = Event::new(
            "ctxd://test".to_string(),
            Subject::new("/temporal/b").unwrap(),
            "demo".to_string(),
            serde_json::json!({"sub": "b"}),
        );
        e2.time = t2;
        store.append(e2).await.unwrap();

        let parent = Subject::new("/temporal").unwrap();
        // At t1 only /temporal/a should be visible
        let events = store.read_at(&parent, t1, true).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].subject.as_str(), "/temporal/a");

        // At t2 both should be visible
        let events = store.read_at(&parent, t2, true).await.unwrap();
        assert_eq!(events.len(), 2);
    }

    #[tokio::test]
    async fn kv_get_at_returns_latest_before_timestamp() {
        use chrono::TimeZone;
        let store = EventStore::open_memory().await.unwrap();
        let subject = Subject::new("/temporal/kv").unwrap();

        let t1 = chrono::Utc.with_ymd_and_hms(2025, 1, 1, 10, 0, 0).unwrap();
        let t2 = chrono::Utc.with_ymd_and_hms(2025, 1, 1, 11, 0, 0).unwrap();
        let t3 = chrono::Utc.with_ymd_and_hms(2025, 1, 1, 12, 0, 0).unwrap();

        for (i, t) in [(1, t1), (2, t2), (3, t3)] {
            let mut event = Event::new(
                "ctxd://test".to_string(),
                subject.clone(),
                "demo".to_string(),
                serde_json::json!({"version": i}),
            );
            event.time = t;
            store.append(event).await.unwrap();
        }

        // At t2, should get version 2 (not version 3)
        let val = store.kv_get_at("/temporal/kv", t2).await.unwrap().unwrap();
        assert_eq!(val, serde_json::json!({"version": 2}));

        // At t1, should get version 1
        let val = store.kv_get_at("/temporal/kv", t1).await.unwrap().unwrap();
        assert_eq!(val, serde_json::json!({"version": 1}));

        // At t3, should get version 3
        let val = store.kv_get_at("/temporal/kv", t3).await.unwrap().unwrap();
        assert_eq!(val, serde_json::json!({"version": 3}));

        // Before t1, should get None
        let t0 = chrono::Utc.with_ymd_and_hms(2025, 1, 1, 9, 0, 0).unwrap();
        let val = store.kv_get_at("/temporal/kv", t0).await.unwrap();
        assert!(val.is_none());
    }
}
