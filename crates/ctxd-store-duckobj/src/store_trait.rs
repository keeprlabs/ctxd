//! `ctxd_store_core::Store` impl for [`crate::DuckObjStore`].
//!
//! Mirrors the Postgres store_trait.rs layout: convert the inner
//! `StoreError` into the shared `StoreError` variant, route trait
//! methods to inherent methods on `DuckObjStore` or to the sidecar.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ctxd_core::event::Event;
use ctxd_core::subject::Subject;
use ctxd_store_core::{
    EntityQuery, EntityRow, Peer, PeerCursor, RelationshipRow, Store, StoreError as CoreStoreError,
    VectorSearchResult,
};

use crate::store::{DuckObjStore, StoreError as DuckErr};

impl From<DuckErr> for CoreStoreError {
    fn from(e: DuckErr) -> Self {
        match e {
            DuckErr::HashChainViolation { expected, actual } => {
                CoreStoreError::HashChainViolation { expected, actual }
            }
            DuckErr::Subject(s) => CoreStoreError::Subject(s),
            DuckErr::Serialization(s) => CoreStoreError::Serialization(s),
            other => CoreStoreError::Other(other.to_string()),
        }
    }
}

#[async_trait]
impl Store for DuckObjStore {
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

    async fn entities_query(&self, _q: &EntityQuery) -> Result<Vec<EntityRow>, CoreStoreError> {
        // Graph view not materialized on this backend in v0.3.
        // Operators wanting graph queries against DuckObj should
        // plug in an external DuckDB process against the Parquet
        // parts; tracked for v0.4.
        Ok(Vec::new())
    }

    async fn relationships_for(
        &self,
        _entity_id: &str,
    ) -> Result<Vec<(RelationshipRow, EntityRow)>, CoreStoreError> {
        Ok(Vec::new())
    }

    async fn peer_add(&self, peer: Peer) -> Result<(), CoreStoreError> {
        self.sidecar().peer_add(peer).await.map_err(Into::into)
    }

    async fn peer_list(&self) -> Result<Vec<Peer>, CoreStoreError> {
        self.sidecar().peer_list().await.map_err(Into::into)
    }

    async fn peer_remove(&self, peer_id: &str) -> Result<(), CoreStoreError> {
        self.sidecar()
            .peer_remove(peer_id)
            .await
            .map_err(Into::into)
    }

    async fn peer_cursor_set(&self, cursor: PeerCursor) -> Result<(), CoreStoreError> {
        self.sidecar()
            .peer_cursor_set(cursor)
            .await
            .map_err(Into::into)
    }

    async fn peer_cursor_get(
        &self,
        peer_id: &str,
        subject_pattern: &str,
    ) -> Result<Option<PeerCursor>, CoreStoreError> {
        self.sidecar()
            .peer_cursor_get(peer_id, subject_pattern)
            .await
            .map_err(Into::into)
    }

    async fn revoke_token(&self, token_id: &str) -> Result<(), CoreStoreError> {
        self.sidecar()
            .revoke_token(token_id)
            .await
            .map_err(Into::into)
    }

    async fn is_token_revoked(&self, token_id: &str) -> Result<bool, CoreStoreError> {
        self.sidecar()
            .is_token_revoked(token_id)
            .await
            .map_err(Into::into)
    }

    async fn vector_upsert(
        &self,
        event_id: &str,
        model: &str,
        vector: &[f32],
    ) -> Result<(), CoreStoreError> {
        self.sidecar()
            .vector_upsert(event_id, model, vector)
            .await
            .map_err(Into::into)
    }

    async fn vector_search(
        &self,
        query: &[f32],
        k: usize,
    ) -> Result<Vec<VectorSearchResult>, CoreStoreError> {
        self.sidecar()
            .vector_search(query, k)
            .await
            .map_err(Into::into)
    }
}
