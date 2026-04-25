//! `ctxd_store_core::Store` impl for [`EventStore`].
//!
//! The concrete `EventStore` API is kept intact so existing call sites
//! keep compiling. New callers that want runtime backend selection go
//! through the trait: `Arc<dyn Store>`.
//!
//! Conversions between the concrete [`crate::StoreError`] and
//! [`ctxd_store_core::StoreError`] are centralized here.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ctxd_core::event::Event;
use ctxd_core::subject::Subject;
use ctxd_store_core::{
    EntityQuery, EntityRow, Peer, PeerCursor, RelationshipRow, Store, StoreError as CoreStoreError,
    VectorSearchResult,
};

use crate::store::{EventStore, StoreError as SqliteStoreError};

impl From<SqliteStoreError> for CoreStoreError {
    fn from(e: SqliteStoreError) -> Self {
        match e {
            SqliteStoreError::HashChainViolation { expected, actual } => {
                CoreStoreError::HashChainViolation { expected, actual }
            }
            SqliteStoreError::Subject(err) => CoreStoreError::Subject(err),
            SqliteStoreError::Serialization(err) => CoreStoreError::Serialization(err),
            SqliteStoreError::Database(err) => CoreStoreError::backend(err),
        }
    }
}

// Additional inherent methods on EventStore for the pieces that weren't
// part of v0.2's public API. Kept in this module so we don't bloat
// `store.rs`, and so the trait impl below reads as straight delegation.
impl EventStore {
    /// Register a federation peer. Idempotent on `peer_id`.
    pub async fn peer_add_impl(&self, peer: Peer) -> Result<(), SqliteStoreError> {
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
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// List all registered peers.
    pub async fn peer_list_impl(&self) -> Result<Vec<Peer>, SqliteStoreError> {
        let rows: Vec<(String, String, Vec<u8>, String, String, String)> = sqlx::query_as(
            "SELECT peer_id, url, public_key, granted_subjects, trust_level, added_at FROM peers ORDER BY added_at",
        )
        .fetch_all(self.pool())
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for (peer_id, url, public_key, granted_subjects, trust_level, added_at) in rows {
            out.push(Peer {
                peer_id,
                url,
                public_key,
                granted_subjects: serde_json::from_str(&granted_subjects)?,
                trust_level: serde_json::from_str(&trust_level)?,
                added_at: parse_ts(&added_at)?,
            });
        }
        Ok(out)
    }

    /// Remove a peer by id. Missing peers are not an error.
    pub async fn peer_remove_impl(&self, peer_id: &str) -> Result<(), SqliteStoreError> {
        sqlx::query("DELETE FROM peers WHERE peer_id = ?")
            .bind(peer_id)
            .execute(self.pool())
            .await?;
        sqlx::query("DELETE FROM peer_cursors WHERE peer_id = ?")
            .bind(peer_id)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// Upsert a replication cursor.
    pub async fn peer_cursor_set_impl(&self, cursor: PeerCursor) -> Result<(), SqliteStoreError> {
        let now = Utc::now().to_rfc3339();
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
        .bind(cursor.last_event_id.map(|u| u.to_string()))
        .bind(cursor.last_event_time.map(|t| t.to_rfc3339()))
        .bind(&now)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Fetch a replication cursor.
    pub async fn peer_cursor_get_impl(
        &self,
        peer_id: &str,
        subject_pattern: &str,
    ) -> Result<Option<PeerCursor>, SqliteStoreError> {
        let row: Option<(String, String, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT peer_id, subject_pattern, last_event_id, last_event_time FROM peer_cursors WHERE peer_id = ? AND subject_pattern = ?",
        )
        .bind(peer_id)
        .bind(subject_pattern)
        .fetch_optional(self.pool())
        .await?;
        match row {
            Some((pid, pattern, last_id, last_time)) => {
                let last_event_id = match last_id {
                    Some(s) => Some(s.parse().map_err(|e: uuid::Error| {
                        SqliteStoreError::Database(sqlx::Error::Protocol(format!(
                            "bad cursor uuid: {e}"
                        )))
                    })?),
                    None => None,
                };
                let last_event_time = match last_time {
                    Some(s) => Some(parse_ts(&s)?),
                    None => None,
                };
                Ok(Some(PeerCursor {
                    peer_id: pid,
                    subject_pattern: pattern,
                    last_event_id,
                    last_event_time,
                }))
            }
            None => Ok(None),
        }
    }

    /// Upsert a vector embedding, persisting the raw floats into the
    /// `vector_embeddings` SQLite table. Storage is little-endian `f32`.
    pub async fn vector_upsert_impl(
        &self,
        event_id: &str,
        model: &str,
        vector: &[f32],
    ) -> Result<(), SqliteStoreError> {
        let mut bytes = Vec::with_capacity(vector.len() * 4);
        for f in vector {
            bytes.extend_from_slice(&f.to_le_bytes());
        }
        let now = Utc::now().to_rfc3339();
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
        .bind(&now)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Top-k vector search.
    ///
    /// Uses the in-memory HNSW index when one has been opened via
    /// [`EventStore::ensure_vector_index`]; otherwise falls back to
    /// a brute-force cosine scan over `vector_embeddings`. The
    /// brute-force path is intentionally still here so unconfigured
    /// stores (e.g. the conformance suite) can exercise the trait
    /// surface without spinning up an HNSW.
    pub async fn vector_search_impl(
        &self,
        query: &[f32],
        k: usize,
    ) -> Result<Vec<VectorSearchResult>, SqliteStoreError> {
        if k == 0 || query.is_empty() {
            return Ok(Vec::new());
        }
        if let Some(idx) = self.vector_index() {
            // Index path — O(log n) thanks to HNSW.
            match idx.search(query, k) {
                Ok(pairs) => {
                    return Ok(pairs
                        .into_iter()
                        .map(|(event_id, score)| VectorSearchResult { event_id, score })
                        .collect())
                }
                Err(e) => {
                    tracing::warn!(error = %e, "HNSW search failed; falling back to brute force");
                }
            }
        }
        let rows: Vec<(String, i64, Vec<u8>)> =
            sqlx::query_as("SELECT event_id, dimensions, vector FROM vector_embeddings")
                .fetch_all(self.pool())
                .await?;
        let mut scored = Vec::with_capacity(rows.len());
        for (event_id, dims, bytes) in rows {
            if (dims as usize) != query.len() {
                continue; // dimension mismatch — skip rather than error
            }
            if bytes.len() != (dims as usize) * 4 {
                continue;
            }
            let mut v = Vec::with_capacity(dims as usize);
            for chunk in bytes.chunks_exact(4) {
                let arr = [chunk[0], chunk[1], chunk[2], chunk[3]];
                v.push(f32::from_le_bytes(arr));
            }
            let score = cosine_distance(query, &v);
            scored.push(VectorSearchResult { event_id, score });
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

fn parse_ts(s: &str) -> Result<DateTime<Utc>, SqliteStoreError> {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|e| SqliteStoreError::Database(sqlx::Error::Protocol(format!("bad time: {e}"))))
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
    let sim = dot / (na.sqrt() * nb.sqrt());
    1.0 - sim
}

#[async_trait]
impl Store for EventStore {
    async fn append(&self, event: Event) -> Result<Event, CoreStoreError> {
        self.append(event).await.map_err(Into::into)
    }

    async fn read(&self, subject: &Subject, recursive: bool) -> Result<Vec<Event>, CoreStoreError> {
        self.read(subject, recursive).await.map_err(Into::into)
    }

    async fn read_at(
        &self,
        subject: &Subject,
        as_of: DateTime<Utc>,
        recursive: bool,
    ) -> Result<Vec<Event>, CoreStoreError> {
        self.read_at(subject, as_of, recursive)
            .await
            .map_err(Into::into)
    }

    async fn subjects(
        &self,
        prefix: Option<&Subject>,
        recursive: bool,
    ) -> Result<Vec<String>, CoreStoreError> {
        self.subjects(prefix, recursive).await.map_err(Into::into)
    }

    async fn search(
        &self,
        query: &str,
        limit: Option<usize>,
    ) -> Result<Vec<Event>, CoreStoreError> {
        self.search(query, limit).await.map_err(Into::into)
    }

    async fn kv_get(&self, subject: &str) -> Result<Option<serde_json::Value>, CoreStoreError> {
        self.kv_get(subject).await.map_err(Into::into)
    }

    async fn kv_get_at(
        &self,
        subject: &str,
        as_of: DateTime<Utc>,
    ) -> Result<Option<serde_json::Value>, CoreStoreError> {
        self.kv_get_at(subject, as_of).await.map_err(Into::into)
    }

    async fn entities_query(&self, q: &EntityQuery) -> Result<Vec<EntityRow>, CoreStoreError> {
        let graph = self.graph_view();
        let entities = match &q.entity_type {
            Some(t) => graph
                .get_entities(Some(t))
                .await
                .map_err(CoreStoreError::from)?,
            None => graph
                .get_entities(None)
                .await
                .map_err(CoreStoreError::from)?,
        };
        let mut rows = Vec::with_capacity(entities.len());
        for e in entities {
            if let Some(needle) = &q.name_contains {
                if !e.name.contains(needle) {
                    continue;
                }
            }
            rows.push(EntityRow {
                id: e.id,
                entity_type: e.entity_type,
                name: e.name,
                properties: e.properties,
                source_event_id: e.source_event_id,
            });
            if let Some(lim) = q.limit {
                if rows.len() >= lim {
                    break;
                }
            }
        }
        Ok(rows)
    }

    async fn relationships_for(
        &self,
        entity_id: &str,
    ) -> Result<Vec<(RelationshipRow, EntityRow)>, CoreStoreError> {
        let graph = self.graph_view();
        let related = graph
            .get_related(entity_id, None)
            .await
            .map_err(CoreStoreError::from)?;
        let mut out = Vec::with_capacity(related.len());
        for (rel, ent) in related {
            out.push((
                RelationshipRow {
                    id: rel.id,
                    from_entity_id: rel.from_entity_id,
                    to_entity_id: rel.to_entity_id,
                    relationship_type: rel.relationship_type,
                    properties: rel.properties,
                    source_event_id: rel.source_event_id,
                },
                EntityRow {
                    id: ent.id,
                    entity_type: ent.entity_type,
                    name: ent.name,
                    properties: ent.properties,
                    source_event_id: ent.source_event_id,
                },
            ));
        }
        Ok(out)
    }

    async fn peer_add(&self, peer: Peer) -> Result<(), CoreStoreError> {
        self.peer_add_impl(peer).await.map_err(Into::into)
    }

    async fn peer_list(&self) -> Result<Vec<Peer>, CoreStoreError> {
        self.peer_list_impl().await.map_err(Into::into)
    }

    async fn peer_remove(&self, peer_id: &str) -> Result<(), CoreStoreError> {
        self.peer_remove_impl(peer_id).await.map_err(Into::into)
    }

    async fn peer_cursor_set(&self, cursor: PeerCursor) -> Result<(), CoreStoreError> {
        self.peer_cursor_set_impl(cursor).await.map_err(Into::into)
    }

    async fn peer_cursor_get(
        &self,
        peer_id: &str,
        subject_pattern: &str,
    ) -> Result<Option<PeerCursor>, CoreStoreError> {
        self.peer_cursor_get_impl(peer_id, subject_pattern)
            .await
            .map_err(Into::into)
    }

    async fn revoke_token(&self, token_id: &str) -> Result<(), CoreStoreError> {
        self.revoke_token(token_id).await.map_err(Into::into)
    }

    async fn is_token_revoked(&self, token_id: &str) -> Result<bool, CoreStoreError> {
        self.is_token_revoked(token_id).await.map_err(Into::into)
    }

    async fn vector_upsert(
        &self,
        event_id: &str,
        model: &str,
        vector: &[f32],
    ) -> Result<(), CoreStoreError> {
        self.vector_upsert_impl(event_id, model, vector)
            .await
            .map_err(Into::into)
    }

    async fn vector_search(
        &self,
        query: &[f32],
        k: usize,
    ) -> Result<Vec<VectorSearchResult>, CoreStoreError> {
        self.vector_search_impl(query, k).await.map_err(Into::into)
    }
}
