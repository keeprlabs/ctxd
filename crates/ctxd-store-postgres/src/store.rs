//! Postgres-backed event store core implementation.
//!
//! The struct exposed here ([`PostgresStore`]) is `Clone` (it wraps a
//! `PgPool` `Arc`-internally) so it can be shared across tasks freely.
//! All `Store` trait wiring lives in `store_trait.rs`.

use chrono::{DateTime, Utc};
use ctxd_core::event::Event;
use ctxd_core::hash::PredecessorHash;
use ctxd_core::signing::EventSigner;
use ctxd_core::subject::Subject;
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::Row;
use uuid::Uuid;

use crate::schema;

/// Errors from the Postgres event store.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// Native sqlx / Postgres error.
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    /// JSON (de)serialization failure.
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// A migration failed.
    #[error("migration {name} failed: {source}")]
    Migration {
        /// Name of the failing migration (e.g. "0001_events").
        name: String,
        /// Underlying sqlx error.
        #[source]
        source: sqlx::Error,
    },

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

    /// Malformed value coming back from Postgres (e.g. a UUID column
    /// that didn't parse, a row that didn't have the expected shape).
    #[error("decode error: {0}")]
    Decode(String),
}

/// Postgres-backed event store.
///
/// Owns a [`PgPool`] and applies migrations at construction time. Cheap
/// to clone: the pool itself is reference-counted internally.
#[derive(Clone, Debug)]
pub struct PostgresStore {
    pool: PgPool,
    signing_key: Option<Vec<u8>>,
}

impl PostgresStore {
    /// Connect to the supplied database URL (e.g.
    /// `postgres://user:pass@host:5432/db`) and apply migrations.
    ///
    /// Uses `PgPoolOptions::default()` for everything except `max_connections`,
    /// which we cap at 10. Callers that need a custom pool should use
    /// [`PostgresStore::with_pool`].
    pub async fn connect(database_url: &str) -> Result<Self, StoreError> {
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .connect(database_url)
            .await?;
        Self::with_pool(pool).await
    }

    /// Build a store on a pre-configured pool.
    ///
    /// Runs the embedded migrations on first call. Subsequent calls
    /// against the same database are no-ops (every migration is
    /// idempotent).
    pub async fn with_pool(pool: PgPool) -> Result<Self, StoreError> {
        schema::run_migrations(&pool).await?;
        Ok(Self {
            pool,
            signing_key: None,
        })
    }

    /// Install an Ed25519 signing key. Future appends without a
    /// pre-baked signature will be signed with this key.
    pub fn set_signing_key(&mut self, key: Vec<u8>) {
        self.signing_key = Some(key);
    }

    /// Access the underlying pool (used by the caveat-state and graph
    /// view modules).
    #[doc(hidden)]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Append an event under the canonical hash-chain rules.
    ///
    /// Concurrency: the per-subject hash chain requires that the read
    /// of "last event for subject X" and the insert of the new event
    /// are atomic with respect to other appenders on the same subject.
    /// We hold a per-subject `pg_advisory_xact_lock` for the duration
    /// of the transaction. The lock key is a stable 64-bit hash of the
    /// subject path so concurrent writers on different subjects don't
    /// contend (see ADR 016).
    #[tracing::instrument(skip(self, event), fields(subject = %event.subject))]
    pub async fn append(&self, mut event: Event) -> Result<Event, StoreError> {
        // Begin transaction up front — the advisory lock and the actual
        // INSERT must share a transaction so the lock is released on
        // commit/rollback automatically.
        let mut tx = self.pool.begin().await?;

        // Subject-keyed advisory lock. Twin BIGINT form (two i32s) is
        // not available via the sqlx high-level API consistently
        // across sqlx versions, so we use the single-BIGINT form with
        // a stable hash. Collisions cause an unrelated subject pair to
        // serialize — correctness is preserved, only throughput is
        // affected, and at 64 bits collisions are astronomical for the
        // populations we anticipate.
        let lock_key = subject_lock_key(event.subject.as_str());
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(lock_key)
            .execute(&mut *tx)
            .await?;

        // Compute predecessor hash if the caller didn't provide one.
        // Replicated events arrive with a predecessorhash baked into
        // their signature — recomputing would invalidate the
        // signature.
        if event.predecessorhash.is_none() {
            let last_event =
                last_event_for_subject_in_tx(&mut tx, event.subject.as_str()).await?;
            if let Some(ref prev) = last_event {
                let hash = PredecessorHash::compute(prev).map_err(StoreError::Serialization)?;
                event.predecessorhash = Some(hash.to_string());
            }
        }

        // Sign the event if a signing key is available AND the event
        // is not already signed. Replicated events arrive pre-signed.
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

        let parents_sorted: Vec<Uuid> = event.parents_sorted();

        // Insert into events. `parents` is a UUID[] column —
        // round-trips losslessly via sqlx.
        sqlx::query(
            r#"
            INSERT INTO events (
                id, source, subject, event_type, time, datacontenttype, data,
                predecessorhash, signature, parents, attestation, specversion
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
            "#,
        )
        .bind(event.id)
        .bind(&event.source)
        .bind(event.subject.as_str())
        .bind(&event.event_type)
        .bind(event.time)
        .bind(&event.datacontenttype)
        .bind(&event.data)
        .bind(event.predecessorhash.as_deref())
        .bind(event.signature.as_deref())
        .bind(&parents_sorted[..])
        .bind(event.attestation.as_deref())
        .bind(&event.specversion)
        .execute(&mut *tx)
        .await?;

        // Side-table for parent-edge lookups.
        for parent in &parents_sorted {
            sqlx::query(
                r#"
                INSERT INTO event_parents (event_id, parent_id)
                VALUES ($1, $2)
                ON CONFLICT (event_id, parent_id) DO NOTHING
                "#,
            )
            .bind(event.id)
            .bind(parent)
            .execute(&mut *tx)
            .await?;
        }

        // KV view under federation LWW (ADR 006). Postgres compares
        // tuples lexicographically the same way SQLite does, so the
        // semantics match byte-for-byte.
        sqlx::query(
            r#"
            INSERT INTO kv_view (subject, event_id, data, time)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT (subject) DO UPDATE SET
                event_id = EXCLUDED.event_id,
                data     = EXCLUDED.data,
                time     = EXCLUDED.time
            WHERE (EXCLUDED.time, EXCLUDED.event_id) > (kv_view.time, kv_view.event_id)
            "#,
        )
        .bind(event.subject.as_str())
        .bind(event.id)
        .bind(&event.data)
        .bind(event.time)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(event)
    }

    /// Read events for a subject, optionally recursive.
    #[tracing::instrument(skip(self), fields(subject = %subject))]
    pub async fn read(
        &self,
        subject: &Subject,
        recursive: bool,
    ) -> Result<Vec<Event>, StoreError> {
        let rows = if recursive {
            let pattern = recursive_like_pattern(subject.as_str());
            sqlx::query(
                r#"
                SELECT id, source, subject, event_type, time, datacontenttype, data,
                       predecessorhash, signature, parents, attestation, specversion
                FROM events
                WHERE subject = $1 OR subject LIKE $2
                ORDER BY seq ASC
                "#,
            )
            .bind(subject.as_str())
            .bind(&pattern)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query(
                r#"
                SELECT id, source, subject, event_type, time, datacontenttype, data,
                       predecessorhash, signature, parents, attestation, specversion
                FROM events
                WHERE subject = $1
                ORDER BY seq ASC
                "#,
            )
            .bind(subject.as_str())
            .fetch_all(&self.pool)
            .await?
        };

        rows.into_iter().map(row_to_event).collect()
    }

    /// Read events for a subject as of a timestamp, optionally recursive.
    pub async fn read_at(
        &self,
        subject: &Subject,
        as_of: DateTime<Utc>,
        recursive: bool,
    ) -> Result<Vec<Event>, StoreError> {
        let rows = if recursive {
            let pattern = recursive_like_pattern(subject.as_str());
            sqlx::query(
                r#"
                SELECT id, source, subject, event_type, time, datacontenttype, data,
                       predecessorhash, signature, parents, attestation, specversion
                FROM events
                WHERE (subject = $1 OR subject LIKE $2) AND time <= $3
                ORDER BY seq ASC
                "#,
            )
            .bind(subject.as_str())
            .bind(&pattern)
            .bind(as_of)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query(
                r#"
                SELECT id, source, subject, event_type, time, datacontenttype, data,
                       predecessorhash, signature, parents, attestation, specversion
                FROM events
                WHERE subject = $1 AND time <= $2
                ORDER BY seq ASC
                "#,
            )
            .bind(subject.as_str())
            .bind(as_of)
            .fetch_all(&self.pool)
            .await?
        };

        rows.into_iter().map(row_to_event).collect()
    }

    /// List distinct subjects, optionally under a prefix.
    pub async fn subjects(
        &self,
        prefix: Option<&Subject>,
        recursive: bool,
    ) -> Result<Vec<String>, StoreError> {
        let rows = if let Some(pfx) = prefix {
            if recursive {
                let pattern = recursive_like_pattern(pfx.as_str());
                sqlx::query(
                    "SELECT DISTINCT subject FROM events WHERE subject = $1 OR subject LIKE $2 ORDER BY subject",
                )
                .bind(pfx.as_str())
                .bind(&pattern)
                .fetch_all(&self.pool)
                .await?
            } else {
                sqlx::query(
                    "SELECT DISTINCT subject FROM events WHERE subject = $1 ORDER BY subject",
                )
                .bind(pfx.as_str())
                .fetch_all(&self.pool)
                .await?
            }
        } else {
            sqlx::query("SELECT DISTINCT subject FROM events ORDER BY subject")
                .fetch_all(&self.pool)
                .await?
        };

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let s: String = row
                .try_get::<String, _>("subject")
                .map_err(|e| StoreError::Decode(format!("subject column: {e}")))?;
            out.push(s);
        }
        Ok(out)
    }

    /// Full-text search via the generated `fts_tsv` column.
    ///
    /// We use `websearch_to_tsquery` (PG 11+) so callers can pass
    /// natural-looking queries like `hello world` or `"exact phrase"`.
    /// Results are ordered by `ts_rank` desc, then by ascending `seq`
    /// for stable ordering when scores tie.
    #[tracing::instrument(skip(self))]
    pub async fn search(
        &self,
        query: &str,
        limit: Option<usize>,
    ) -> Result<Vec<Event>, StoreError> {
        // Postgres has no LIMIT-NULL syntax — we branch the SQL.
        let rows = if let Some(lim) = limit {
            sqlx::query(
                r#"
                SELECT id, source, subject, event_type, time, datacontenttype, data,
                       predecessorhash, signature, parents, attestation, specversion
                FROM events
                WHERE fts_tsv @@ websearch_to_tsquery('english', $1)
                ORDER BY ts_rank(fts_tsv, websearch_to_tsquery('english', $1)) DESC, seq ASC
                LIMIT $2
                "#,
            )
            .bind(query)
            .bind(lim as i64)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query(
                r#"
                SELECT id, source, subject, event_type, time, datacontenttype, data,
                       predecessorhash, signature, parents, attestation, specversion
                FROM events
                WHERE fts_tsv @@ websearch_to_tsquery('english', $1)
                ORDER BY ts_rank(fts_tsv, websearch_to_tsquery('english', $1)) DESC, seq ASC
                "#,
            )
            .bind(query)
            .fetch_all(&self.pool)
            .await?
        };

        rows.into_iter().map(row_to_event).collect()
    }

    /// Latest KV value for a subject.
    pub async fn kv_get(&self, subject: &str) -> Result<Option<serde_json::Value>, StoreError> {
        let row = sqlx::query("SELECT data FROM kv_view WHERE subject = $1")
            .bind(subject)
            .fetch_optional(&self.pool)
            .await?;
        match row {
            Some(r) => {
                let v: serde_json::Value = r
                    .try_get("data")
                    .map_err(|e| StoreError::Decode(format!("kv_view.data: {e}")))?;
                Ok(Some(v))
            }
            None => Ok(None),
        }
    }

    /// KV value for a subject as of a timestamp. Reconstructs from the
    /// raw event log so the answer is correct even if the materialized
    /// `kv_view` lags (which it can under federated multi-writer LWW).
    pub async fn kv_get_at(
        &self,
        subject: &str,
        as_of: DateTime<Utc>,
    ) -> Result<Option<serde_json::Value>, StoreError> {
        let row = sqlx::query(
            "SELECT data FROM events WHERE subject = $1 AND time <= $2 ORDER BY seq DESC LIMIT 1",
        )
        .bind(subject)
        .bind(as_of)
        .fetch_optional(&self.pool)
        .await?;
        match row {
            Some(r) => {
                let v: serde_json::Value = r
                    .try_get("data")
                    .map_err(|e| StoreError::Decode(format!("events.data: {e}")))?;
                Ok(Some(v))
            }
            None => Ok(None),
        }
    }

    /// Revoke a token by id. Idempotent.
    pub async fn revoke_token(&self, token_id: &str) -> Result<(), StoreError> {
        sqlx::query(
            r#"
            INSERT INTO revoked_tokens (token_id, revoked_at)
            VALUES ($1, $2)
            ON CONFLICT (token_id) DO NOTHING
            "#,
        )
        .bind(token_id)
        .bind(Utc::now())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Check whether a token is revoked.
    pub async fn is_token_revoked(&self, token_id: &str) -> Result<bool, StoreError> {
        let row = sqlx::query("SELECT 1 FROM revoked_tokens WHERE token_id = $1")
            .bind(token_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.is_some())
    }

    /// Get a metadata value by key.
    pub async fn get_metadata(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
        let row = sqlx::query("SELECT value FROM metadata WHERE key = $1")
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        match row {
            Some(r) => {
                let v: Vec<u8> = r
                    .try_get("value")
                    .map_err(|e| StoreError::Decode(format!("metadata.value: {e}")))?;
                Ok(Some(v))
            }
            None => Ok(None),
        }
    }

    /// Set a metadata value.
    pub async fn set_metadata(&self, key: &str, value: &[u8]) -> Result<(), StoreError> {
        sqlx::query(
            r#"
            INSERT INTO metadata (key, value) VALUES ($1, $2)
            ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value
            "#,
        )
        .bind(key)
        .bind(value)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

/// Build the LIKE pattern for a recursive subject-prefix query.
///
/// The root subject `/` becomes `/%` (matches everything except the
/// root itself, which is added to the OR). Any other subject `/foo`
/// becomes `/foo/%` so we don't accidentally match `/foobar`.
fn recursive_like_pattern(subject: &str) -> String {
    if subject == "/" {
        "/%".to_string()
    } else {
        format!("{subject}/%")
    }
}

/// Stable 64-bit hash of a subject path used as the advisory-lock key.
///
/// We use the FNV-1a 64-bit hash because the standard library hasher
/// is not stable across runs, and we want collisions to be deterministic
/// across daemon restarts so a debugger investigating "why are these two
/// subjects fighting?" can reproduce the situation. FNV-1a is good
/// enough for this — collisions are extremely rare for the population
/// of subjects we expect (low millions).
fn subject_lock_key(subject: &str) -> i64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x100_0000_01b3;
    let mut hash = FNV_OFFSET;
    for byte in subject.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    // Reinterpret as i64 — Postgres advisory locks take a BIGINT.
    // Wrap-around is intentional and harmless.
    hash as i64
}

/// Look up the most recent event for a subject inside an open
/// transaction. Used during `append` to compute the predecessor hash.
async fn last_event_for_subject_in_tx<'c>(
    tx: &mut sqlx::Transaction<'c, sqlx::Postgres>,
    subject: &str,
) -> Result<Option<Event>, StoreError> {
    let row = sqlx::query(
        r#"
        SELECT id, source, subject, event_type, time, datacontenttype, data,
               predecessorhash, signature, parents, attestation, specversion
        FROM events
        WHERE subject = $1
        ORDER BY seq DESC
        LIMIT 1
        "#,
    )
    .bind(subject)
    .fetch_optional(&mut **tx)
    .await?;
    match row {
        Some(r) => Ok(Some(row_to_event(r)?)),
        None => Ok(None),
    }
}

/// Convert a raw row to an [`Event`]. Centralized so additive schema
/// changes touch one place.
pub(crate) fn row_to_event(row: sqlx::postgres::PgRow) -> Result<Event, StoreError> {
    let id: Uuid = row
        .try_get("id")
        .map_err(|e| StoreError::Decode(format!("events.id: {e}")))?;
    let source: String = row
        .try_get("source")
        .map_err(|e| StoreError::Decode(format!("events.source: {e}")))?;
    let subject: String = row
        .try_get("subject")
        .map_err(|e| StoreError::Decode(format!("events.subject: {e}")))?;
    let event_type: String = row
        .try_get("event_type")
        .map_err(|e| StoreError::Decode(format!("events.event_type: {e}")))?;
    let time: DateTime<Utc> = row
        .try_get("time")
        .map_err(|e| StoreError::Decode(format!("events.time: {e}")))?;
    let datacontenttype: String = row
        .try_get("datacontenttype")
        .map_err(|e| StoreError::Decode(format!("events.datacontenttype: {e}")))?;
    let data: serde_json::Value = row
        .try_get("data")
        .map_err(|e| StoreError::Decode(format!("events.data: {e}")))?;
    let predecessorhash: Option<String> = row
        .try_get("predecessorhash")
        .map_err(|e| StoreError::Decode(format!("events.predecessorhash: {e}")))?;
    let signature: Option<String> = row
        .try_get("signature")
        .map_err(|e| StoreError::Decode(format!("events.signature: {e}")))?;
    let parents: Vec<Uuid> = row
        .try_get("parents")
        .map_err(|e| StoreError::Decode(format!("events.parents: {e}")))?;
    let attestation: Option<Vec<u8>> = row
        .try_get("attestation")
        .map_err(|e| StoreError::Decode(format!("events.attestation: {e}")))?;
    let specversion: String = row
        .try_get("specversion")
        .map_err(|e| StoreError::Decode(format!("events.specversion: {e}")))?;

    Ok(Event {
        specversion,
        id,
        source,
        subject: Subject::new(&subject)?,
        event_type,
        time,
        datacontenttype,
        data,
        predecessorhash,
        signature,
        parents,
        attestation,
    })
}
