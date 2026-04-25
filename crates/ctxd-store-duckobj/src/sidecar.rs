//! SQLite sidecar for the DuckObj backend.
//!
//! Holds the small, transactional views Parquet is bad at: KV-view,
//! peers, peer_cursors, revoked_tokens, vector_embeddings, plus a
//! lightweight by-id reference for unflushed events so restart cost
//! is bounded.
//!
//! Schema is a subset of `ctxd-store-sqlite` — we deliberately
//! mirror it so federation-LWW semantics match byte-for-byte across
//! backends.

use std::path::Path;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use ctxd_core::event::Event;
use ctxd_store_core::{Peer, PeerCursor, VectorSearchResult};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};
use sqlx::Row;
use uuid::Uuid;

use crate::store::StoreError;

/// SQLite-backed sidecar. Cheap to clone via an `Arc` on the
/// outside.
#[derive(Debug)]
pub struct Sidecar {
    pool: SqlitePool,
}

impl Sidecar {
    /// Open or create the sidecar database at `path`.
    ///
    /// Uses WAL journaling + a generous `busy_timeout` so concurrent
    /// writers on different subjects don't collide on the global
    /// `database is locked` error. This matters because the DuckObj
    /// store's hot loop is `append() -> kv_upsert_lww() ->
    /// record_event_ref()`, three writes per event under a tight
    /// concurrent workload.
    pub async fn open(path: &Path) -> Result<Self, StoreError> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let url = format!("sqlite:{}?mode=rwc", path.display());
        let opts = SqliteConnectOptions::from_str(&url)?
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(std::time::Duration::from_secs(30))
            .pragma("synchronous", "NORMAL");
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(opts)
            .await?;
        let s = Self { pool };
        s.migrate().await?;
        Ok(s)
    }

    async fn migrate(&self) -> Result<(), StoreError> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS kv_view (
                subject TEXT PRIMARY KEY,
                event_id TEXT NOT NULL,
                data TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS peers (
                peer_id TEXT PRIMARY KEY,
                url TEXT NOT NULL,
                public_key BLOB NOT NULL,
                granted_subjects TEXT NOT NULL DEFAULT '[]',
                trust_level TEXT NOT NULL DEFAULT '{}',
                added_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS peer_cursors (
                peer_id TEXT NOT NULL,
                subject_pattern TEXT NOT NULL,
                last_event_id TEXT,
                last_event_time TEXT,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (peer_id, subject_pattern)
            );
            CREATE TABLE IF NOT EXISTS revoked_tokens (
                token_id TEXT PRIMARY KEY,
                revoked_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS vector_embeddings (
                event_id TEXT PRIMARY KEY,
                model TEXT NOT NULL,
                dimensions INTEGER NOT NULL,
                vector BLOB NOT NULL,
                created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS event_refs (
                seq INTEGER PRIMARY KEY,
                event_id TEXT NOT NULL,
                subject TEXT NOT NULL,
                time TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_event_refs_eid ON event_refs(event_id);
            "#,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Upsert the KV-view row under the federation LWW rule (ADR 006).
    pub async fn kv_upsert_lww(
        &self,
        subject: &str,
        event_id: Uuid,
        data: &serde_json::Value,
        time: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let eid = event_id.to_string();
        let data_str = serde_json::to_string(data)?;
        let time_str = time.to_rfc3339();
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
        .bind(subject)
        .bind(&eid)
        .bind(&data_str)
        .bind(&time_str)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Record a lightweight reference to an in-buffer event. Used
    /// only for the read-your-writes side-view; not authoritative.
    pub async fn record_event_ref(&self, seq: i64, event: &Event) -> Result<(), StoreError> {
        sqlx::query(
            r#"
            INSERT OR REPLACE INTO event_refs (seq, event_id, subject, time)
            VALUES (?, ?, ?, ?)
            "#,
        )
        .bind(seq)
        .bind(event.id.to_string())
        .bind(event.subject.as_str())
        .bind(event.time.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Latest KV value for a subject.
    pub async fn kv_get(&self, subject: &str) -> Result<Option<serde_json::Value>, StoreError> {
        let row: Option<(String,)> = sqlx::query_as("SELECT data FROM kv_view WHERE subject = ?")
            .bind(subject)
            .fetch_optional(&self.pool)
            .await?;
        match row {
            Some((s,)) => Ok(Some(serde_json::from_str(&s)?)),
            None => Ok(None),
        }
    }

    /// Register or update a peer.
    pub async fn peer_add(&self, peer: Peer) -> Result<(), StoreError> {
        let granted = serde_json::to_string(&peer.granted_subjects)?;
        let trust = serde_json::to_string(&peer.trust_level)?;
        sqlx::query(
            r#"
            INSERT INTO peers (peer_id, url, public_key, granted_subjects, trust_level, added_at)
            VALUES (?, ?, ?, ?, ?, ?)
            ON CONFLICT(peer_id) DO UPDATE SET
                url = excluded.url,
                public_key = excluded.public_key,
                granted_subjects = excluded.granted_subjects,
                trust_level = excluded.trust_level
            "#,
        )
        .bind(&peer.peer_id)
        .bind(&peer.url)
        .bind(&peer.public_key)
        .bind(&granted)
        .bind(&trust)
        .bind(peer.added_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// List all peers.
    pub async fn peer_list(&self) -> Result<Vec<Peer>, StoreError> {
        let rows = sqlx::query(
            "SELECT peer_id, url, public_key, granted_subjects, trust_level, added_at FROM peers ORDER BY added_at",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let peer_id: String = r
                .try_get("peer_id")
                .map_err(|e| StoreError::Decode(format!("peer_id: {e}")))?;
            let url: String = r
                .try_get("url")
                .map_err(|e| StoreError::Decode(format!("url: {e}")))?;
            let public_key: Vec<u8> = r
                .try_get("public_key")
                .map_err(|e| StoreError::Decode(format!("public_key: {e}")))?;
            let granted: String = r
                .try_get("granted_subjects")
                .map_err(|e| StoreError::Decode(format!("granted: {e}")))?;
            let trust: String = r
                .try_get("trust_level")
                .map_err(|e| StoreError::Decode(format!("trust: {e}")))?;
            let added_at: String = r
                .try_get("added_at")
                .map_err(|e| StoreError::Decode(format!("added_at: {e}")))?;
            let granted_subjects: Vec<String> = serde_json::from_str(&granted)?;
            let trust_level: serde_json::Value = serde_json::from_str(&trust)?;
            let added_at = DateTime::parse_from_rfc3339(&added_at)
                .map_err(|e| StoreError::Decode(format!("added_at parse: {e}")))?
                .with_timezone(&Utc);
            out.push(Peer {
                peer_id,
                url,
                public_key,
                granted_subjects,
                trust_level,
                added_at,
            });
        }
        Ok(out)
    }

    /// Remove a peer and all its cursors.
    pub async fn peer_remove(&self, peer_id: &str) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM peer_cursors WHERE peer_id = ?")
            .bind(peer_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM peers WHERE peer_id = ?")
            .bind(peer_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Upsert a peer cursor.
    pub async fn peer_cursor_set(&self, cursor: PeerCursor) -> Result<(), StoreError> {
        let lid = cursor.last_event_id.map(|u| u.to_string());
        let lts = cursor.last_event_time.map(|t| t.to_rfc3339());
        sqlx::query(
            r#"
            INSERT INTO peer_cursors (peer_id, subject_pattern, last_event_id, last_event_time, updated_at)
            VALUES (?, ?, ?, ?, ?)
            ON CONFLICT(peer_id, subject_pattern) DO UPDATE SET
                last_event_id = excluded.last_event_id,
                last_event_time = excluded.last_event_time,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(&cursor.peer_id)
        .bind(&cursor.subject_pattern)
        .bind(&lid)
        .bind(&lts)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Fetch a peer cursor.
    pub async fn peer_cursor_get(
        &self,
        peer_id: &str,
        subject_pattern: &str,
    ) -> Result<Option<PeerCursor>, StoreError> {
        let row = sqlx::query(
            "SELECT peer_id, subject_pattern, last_event_id, last_event_time FROM peer_cursors WHERE peer_id = ? AND subject_pattern = ?",
        )
        .bind(peer_id)
        .bind(subject_pattern)
        .fetch_optional(&self.pool)
        .await?;
        match row {
            Some(r) => {
                let pid: String = r
                    .try_get("peer_id")
                    .map_err(|e| StoreError::Decode(format!("peer_id: {e}")))?;
                let sp: String = r
                    .try_get("subject_pattern")
                    .map_err(|e| StoreError::Decode(format!("subject_pattern: {e}")))?;
                let lid: Option<String> = r
                    .try_get("last_event_id")
                    .map_err(|e| StoreError::Decode(format!("last_event_id: {e}")))?;
                let lts: Option<String> = r
                    .try_get("last_event_time")
                    .map_err(|e| StoreError::Decode(format!("last_event_time: {e}")))?;
                let last_event_id = match lid {
                    Some(s) => Some(
                        Uuid::parse_str(&s)
                            .map_err(|e| StoreError::Decode(format!("uuid: {e}")))?,
                    ),
                    None => None,
                };
                let last_event_time = match lts {
                    Some(s) => Some(
                        DateTime::parse_from_rfc3339(&s)
                            .map_err(|e| StoreError::Decode(format!("last_time: {e}")))?
                            .with_timezone(&Utc),
                    ),
                    None => None,
                };
                Ok(Some(PeerCursor {
                    peer_id: pid,
                    subject_pattern: sp,
                    last_event_id,
                    last_event_time,
                }))
            }
            None => Ok(None),
        }
    }

    /// Mark a token as revoked. Idempotent.
    pub async fn revoke_token(&self, token_id: &str) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO revoked_tokens (token_id, revoked_at) VALUES (?, ?) ON CONFLICT(token_id) DO NOTHING",
        )
        .bind(token_id)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Check revocation.
    pub async fn is_token_revoked(&self, token_id: &str) -> Result<bool, StoreError> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT token_id FROM revoked_tokens WHERE token_id = ?")
                .bind(token_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.is_some())
    }

    /// Upsert a vector embedding. Vectors are stored as raw little-
    /// endian f32 bytes so a future faiss/hnsw cutover can decode
    /// without a data migration.
    pub async fn vector_upsert(
        &self,
        event_id: &str,
        model: &str,
        vector: &[f32],
    ) -> Result<(), StoreError> {
        let mut bytes = Vec::with_capacity(vector.len() * 4);
        for f in vector {
            bytes.extend_from_slice(&f.to_le_bytes());
        }
        sqlx::query(
            r#"
            INSERT INTO vector_embeddings (event_id, model, dimensions, vector, created_at)
            VALUES (?, ?, ?, ?, ?)
            ON CONFLICT(event_id) DO UPDATE SET
                model = excluded.model,
                dimensions = excluded.dimensions,
                vector = excluded.vector,
                created_at = excluded.created_at
            "#,
        )
        .bind(event_id)
        .bind(model)
        .bind(vector.len() as i64)
        .bind(&bytes)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Brute-force cosine-distance top-k.
    pub async fn vector_search(
        &self,
        query: &[f32],
        k: usize,
    ) -> Result<Vec<VectorSearchResult>, StoreError> {
        if k == 0 || query.is_empty() {
            return Ok(Vec::new());
        }
        let rows = sqlx::query("SELECT event_id, dimensions, vector FROM vector_embeddings")
            .fetch_all(&self.pool)
            .await?;
        let mut scored = Vec::with_capacity(rows.len());
        for row in rows {
            let event_id: String = row
                .try_get("event_id")
                .map_err(|e| StoreError::Decode(format!("event_id: {e}")))?;
            let dims: i64 = row
                .try_get("dimensions")
                .map_err(|e| StoreError::Decode(format!("dimensions: {e}")))?;
            let bytes: Vec<u8> = row
                .try_get("vector")
                .map_err(|e| StoreError::Decode(format!("vector: {e}")))?;
            let dims_us = dims as usize;
            if dims_us != query.len() || bytes.len() != dims_us * 4 {
                continue;
            }
            let mut v = Vec::with_capacity(dims_us);
            for chunk in bytes.chunks_exact(4) {
                v.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
            }
            scored.push(VectorSearchResult {
                event_id,
                score: cosine_distance(query, &v),
            });
        }
        scored.sort_by(|a, b| {
            a.score
                .partial_cmp(&b.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(k);
        Ok(scored)
    }
}

fn cosine_distance(a: &[f32], b: &[f32]) -> f32 {
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return 1.0;
    }
    1.0 - dot / (na.sqrt() * nb.sqrt())
}
