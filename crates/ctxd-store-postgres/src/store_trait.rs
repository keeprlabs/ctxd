//! `ctxd_store_core::Store` impl for [`PostgresStore`] plus the
//! ancillary peer / graph / vector inherent helpers the trait routes
//! to.
//!
//! Conversions between the concrete [`crate::StoreError`] and the
//! shared [`ctxd_store_core::StoreError`] are centralized here.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ctxd_core::event::Event;
use ctxd_core::subject::Subject;
use ctxd_store_core::{
    EntityQuery, EntityRow, Peer, PeerCursor, RelationshipRow, Store,
    StoreError as CoreStoreError, VectorSearchResult,
};
use sqlx::Row;
use uuid::Uuid;

use crate::store::{PostgresStore, StoreError as PgStoreError};

impl From<PgStoreError> for CoreStoreError {
    fn from(e: PgStoreError) -> Self {
        match e {
            PgStoreError::HashChainViolation { expected, actual } => {
                CoreStoreError::HashChainViolation { expected, actual }
            }
            PgStoreError::Subject(err) => CoreStoreError::Subject(err),
            PgStoreError::Serialization(err) => CoreStoreError::Serialization(err),
            PgStoreError::Database(err) => CoreStoreError::backend(err),
            PgStoreError::Migration { name, source } => {
                CoreStoreError::Other(format!("migration {name} failed: {source}"))
            }
            PgStoreError::Decode(msg) => CoreStoreError::Other(format!("decode error: {msg}")),
        }
    }
}

impl PostgresStore {
    /// Register a federation peer. Idempotent on `peer_id`.
    pub async fn peer_add_impl(&self, peer: Peer) -> Result<(), PgStoreError> {
        let granted = serde_json::to_value(&peer.granted_subjects)?;
        sqlx::query(
            r#"
            INSERT INTO peers (peer_id, url, public_key, granted_subjects, trust_level, added_at)
            VALUES ($1, $2, $3, $4, $5, $6)
            ON CONFLICT (peer_id) DO UPDATE SET
                url              = EXCLUDED.url,
                public_key       = EXCLUDED.public_key,
                granted_subjects = EXCLUDED.granted_subjects,
                trust_level      = EXCLUDED.trust_level
            "#,
        )
        .bind(&peer.peer_id)
        .bind(&peer.url)
        .bind(&peer.public_key)
        .bind(&granted)
        .bind(&peer.trust_level)
        .bind(peer.added_at)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// List all registered peers.
    pub async fn peer_list_impl(&self) -> Result<Vec<Peer>, PgStoreError> {
        let rows = sqlx::query(
            "SELECT peer_id, url, public_key, granted_subjects, trust_level, added_at FROM peers ORDER BY added_at",
        )
        .fetch_all(self.pool())
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let peer_id: String = r
                .try_get("peer_id")
                .map_err(|e| PgStoreError::Decode(format!("peers.peer_id: {e}")))?;
            let url: String = r
                .try_get("url")
                .map_err(|e| PgStoreError::Decode(format!("peers.url: {e}")))?;
            let public_key: Vec<u8> = r
                .try_get("public_key")
                .map_err(|e| PgStoreError::Decode(format!("peers.public_key: {e}")))?;
            let granted: serde_json::Value = r
                .try_get("granted_subjects")
                .map_err(|e| PgStoreError::Decode(format!("peers.granted_subjects: {e}")))?;
            let trust_level: serde_json::Value = r
                .try_get("trust_level")
                .map_err(|e| PgStoreError::Decode(format!("peers.trust_level: {e}")))?;
            let added_at: DateTime<Utc> = r
                .try_get("added_at")
                .map_err(|e| PgStoreError::Decode(format!("peers.added_at: {e}")))?;
            let granted_subjects: Vec<String> =
                serde_json::from_value(granted).map_err(PgStoreError::Serialization)?;
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

    /// Remove a peer and any cursors associated with it.
    pub async fn peer_remove_impl(&self, peer_id: &str) -> Result<(), PgStoreError> {
        let mut tx = self.pool().begin().await?;
        sqlx::query("DELETE FROM peer_cursors WHERE peer_id = $1")
            .bind(peer_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM peers WHERE peer_id = $1")
            .bind(peer_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Upsert a replication cursor.
    pub async fn peer_cursor_set_impl(&self, cursor: PeerCursor) -> Result<(), PgStoreError> {
        sqlx::query(
            r#"
            INSERT INTO peer_cursors (peer_id, subject_pattern, last_event_id, last_event_time, updated_at)
            VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT (peer_id, subject_pattern) DO UPDATE SET
                last_event_id   = EXCLUDED.last_event_id,
                last_event_time = EXCLUDED.last_event_time,
                updated_at      = EXCLUDED.updated_at
            "#,
        )
        .bind(&cursor.peer_id)
        .bind(&cursor.subject_pattern)
        .bind(cursor.last_event_id)
        .bind(cursor.last_event_time)
        .bind(Utc::now())
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Fetch a replication cursor.
    pub async fn peer_cursor_get_impl(
        &self,
        peer_id: &str,
        subject_pattern: &str,
    ) -> Result<Option<PeerCursor>, PgStoreError> {
        let row = sqlx::query(
            "SELECT peer_id, subject_pattern, last_event_id, last_event_time FROM peer_cursors WHERE peer_id = $1 AND subject_pattern = $2",
        )
        .bind(peer_id)
        .bind(subject_pattern)
        .fetch_optional(self.pool())
        .await?;
        match row {
            Some(r) => {
                let pid: String = r
                    .try_get("peer_id")
                    .map_err(|e| PgStoreError::Decode(format!("cursor.peer_id: {e}")))?;
                let pattern: String = r
                    .try_get("subject_pattern")
                    .map_err(|e| PgStoreError::Decode(format!("cursor.subject_pattern: {e}")))?;
                let last_event_id: Option<Uuid> = r
                    .try_get("last_event_id")
                    .map_err(|e| PgStoreError::Decode(format!("cursor.last_event_id: {e}")))?;
                let last_event_time: Option<DateTime<Utc>> = r
                    .try_get("last_event_time")
                    .map_err(|e| PgStoreError::Decode(format!("cursor.last_event_time: {e}")))?;
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

    /// Fetch entities matching a query. Mirrors the SQLite impl —
    /// post-filters by `name_contains` substring after the type
    /// filter has already narrowed the candidate set.
    pub async fn entities_query_impl(
        &self,
        q: &EntityQuery,
    ) -> Result<Vec<EntityRow>, PgStoreError> {
        let rows = match &q.entity_type {
            Some(t) => sqlx::query(
                "SELECT id, entity_type, name, properties, source_event_id FROM graph_entities WHERE entity_type = $1 ORDER BY name",
            )
            .bind(t)
            .fetch_all(self.pool())
            .await?,
            None => sqlx::query(
                "SELECT id, entity_type, name, properties, source_event_id FROM graph_entities ORDER BY name",
            )
            .fetch_all(self.pool())
            .await?,
        };

        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let id: String = r
                .try_get("id")
                .map_err(|e| PgStoreError::Decode(format!("entity.id: {e}")))?;
            let entity_type: String = r
                .try_get("entity_type")
                .map_err(|e| PgStoreError::Decode(format!("entity.entity_type: {e}")))?;
            let name: String = r
                .try_get("name")
                .map_err(|e| PgStoreError::Decode(format!("entity.name: {e}")))?;
            let properties: serde_json::Value = r
                .try_get("properties")
                .map_err(|e| PgStoreError::Decode(format!("entity.properties: {e}")))?;
            let source_event_id: String = r
                .try_get("source_event_id")
                .map_err(|e| PgStoreError::Decode(format!("entity.source_event_id: {e}")))?;

            if let Some(needle) = &q.name_contains {
                if !name.contains(needle) {
                    continue;
                }
            }
            out.push(EntityRow {
                id,
                entity_type,
                name,
                properties,
                source_event_id,
            });
            if let Some(lim) = q.limit {
                if out.len() >= lim {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// Walk both incoming and outgoing edges from `entity_id`.
    pub async fn relationships_for_impl(
        &self,
        entity_id: &str,
    ) -> Result<Vec<(RelationshipRow, EntityRow)>, PgStoreError> {
        // Outgoing edges — relationship's "to" entity is the neighbor.
        let outgoing = sqlx::query(
            r#"
            SELECT r.id, r.from_entity_id, r.to_entity_id, r.relationship_type,
                   r.properties, r.source_event_id,
                   e.id as eid, e.entity_type, e.name,
                   e.properties as eprops, e.source_event_id as esrc
            FROM graph_relationships r
            JOIN graph_entities e ON e.id = r.to_entity_id
            WHERE r.from_entity_id = $1
            "#,
        )
        .bind(entity_id)
        .fetch_all(self.pool())
        .await?;

        // Incoming edges — relationship's "from" entity is the neighbor.
        let incoming = sqlx::query(
            r#"
            SELECT r.id, r.from_entity_id, r.to_entity_id, r.relationship_type,
                   r.properties, r.source_event_id,
                   e.id as eid, e.entity_type, e.name,
                   e.properties as eprops, e.source_event_id as esrc
            FROM graph_relationships r
            JOIN graph_entities e ON e.id = r.from_entity_id
            WHERE r.to_entity_id = $1
            "#,
        )
        .bind(entity_id)
        .fetch_all(self.pool())
        .await?;

        let mut out = Vec::with_capacity(outgoing.len() + incoming.len());
        for r in outgoing.into_iter().chain(incoming.into_iter()) {
            let id: String = r
                .try_get("id")
                .map_err(|e| PgStoreError::Decode(format!("rel.id: {e}")))?;
            let from_entity_id: String = r
                .try_get("from_entity_id")
                .map_err(|e| PgStoreError::Decode(format!("rel.from: {e}")))?;
            let to_entity_id: String = r
                .try_get("to_entity_id")
                .map_err(|e| PgStoreError::Decode(format!("rel.to: {e}")))?;
            let relationship_type: String = r
                .try_get("relationship_type")
                .map_err(|e| PgStoreError::Decode(format!("rel.type: {e}")))?;
            let properties: serde_json::Value = r
                .try_get("properties")
                .map_err(|e| PgStoreError::Decode(format!("rel.properties: {e}")))?;
            let source_event_id: String = r
                .try_get("source_event_id")
                .map_err(|e| PgStoreError::Decode(format!("rel.source_event_id: {e}")))?;
            let eid: String = r
                .try_get("eid")
                .map_err(|e| PgStoreError::Decode(format!("entity.id (joined): {e}")))?;
            let entity_type: String = r
                .try_get("entity_type")
                .map_err(|e| PgStoreError::Decode(format!("entity.entity_type (joined): {e}")))?;
            let name: String = r
                .try_get("name")
                .map_err(|e| PgStoreError::Decode(format!("entity.name (joined): {e}")))?;
            let eprops: serde_json::Value = r
                .try_get("eprops")
                .map_err(|e| PgStoreError::Decode(format!("entity.properties (joined): {e}")))?;
            let esrc: String = r
                .try_get("esrc")
                .map_err(|e| PgStoreError::Decode(format!("entity.source_event_id (joined): {e}")))?;

            out.push((
                RelationshipRow {
                    id,
                    from_entity_id,
                    to_entity_id,
                    relationship_type,
                    properties,
                    source_event_id,
                },
                EntityRow {
                    id: eid,
                    entity_type,
                    name,
                    properties: eprops,
                    source_event_id: esrc,
                },
            ));
        }
        Ok(out)
    }
}

#[async_trait]
impl Store for PostgresStore {
    async fn append(&self, event: Event) -> Result<Event, CoreStoreError> {
        self.append(event).await.map_err(Into::into)
    }

    async fn read(
        &self,
        subject: &Subject,
        recursive: bool,
    ) -> Result<Vec<Event>, CoreStoreError> {
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
        self.entities_query_impl(q).await.map_err(Into::into)
    }

    async fn relationships_for(
        &self,
        entity_id: &str,
    ) -> Result<Vec<(RelationshipRow, EntityRow)>, CoreStoreError> {
        self.relationships_for_impl(entity_id)
            .await
            .map_err(Into::into)
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
