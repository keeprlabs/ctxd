//! Vector embeddings: BYTEA storage + brute-force cosine search.
//!
//! Storage is identical in spirit to `ctxd-store-sqlite`'s vector path:
//! we keep raw little-endian f32 bytes in a `BYTEA` column so a future
//! pgvector migration (v0.4) can decode them in place without a data
//! migration.
//!
//! The current search path is a linear scan with cosine distance —
//! same as the SQLite reference impl. This is intentional: it lets the
//! conformance tests pin behavior across both backends, and it avoids
//! pulling in pgvector before we've decided whether it's the right
//! long-term answer (some operators ship Postgres without superuser
//! and can't install extensions).

use crate::store::{PostgresStore, StoreError};
use chrono::Utc;
use ctxd_store_core::VectorSearchResult;
use sqlx::Row;
use uuid::Uuid;

impl PostgresStore {
    /// Persist a vector embedding for an event. Idempotent on `event_id`.
    ///
    /// Vectors are stored as raw `f32` little-endian bytes so a future
    /// pgvector cutover can decode them without a data migration.
    pub async fn vector_upsert_impl(
        &self,
        event_id: &str,
        model: &str,
        vector: &[f32],
    ) -> Result<(), StoreError> {
        let id = Uuid::parse_str(event_id)
            .map_err(|e| StoreError::Decode(format!("vector_upsert event_id: {e}")))?;
        let mut bytes = Vec::with_capacity(vector.len() * 4);
        for f in vector {
            bytes.extend_from_slice(&f.to_le_bytes());
        }
        sqlx::query(
            r#"
            INSERT INTO vector_embeddings (event_id, model, dimensions, vector, created_at)
            VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT (event_id) DO UPDATE SET
                model      = EXCLUDED.model,
                dimensions = EXCLUDED.dimensions,
                vector     = EXCLUDED.vector,
                created_at = EXCLUDED.created_at
            "#,
        )
        .bind(id)
        .bind(model)
        .bind(vector.len() as i32)
        .bind(&bytes)
        .bind(Utc::now())
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Brute-force top-k cosine-distance scan.
    ///
    /// Cheap for a few thousand vectors. For larger workloads we
    /// expect callers to enable pgvector (v0.4) — until then this
    /// matches the SQLite reference impl and keeps the conformance
    /// suite identical across backends.
    pub async fn vector_search_impl(
        &self,
        query: &[f32],
        k: usize,
    ) -> Result<Vec<VectorSearchResult>, StoreError> {
        if k == 0 || query.is_empty() {
            return Ok(Vec::new());
        }

        let rows = sqlx::query(
            "SELECT event_id, dimensions, vector FROM vector_embeddings",
        )
        .fetch_all(self.pool())
        .await?;

        let mut scored = Vec::with_capacity(rows.len());
        for row in rows {
            let event_id: Uuid = row
                .try_get("event_id")
                .map_err(|e| StoreError::Decode(format!("vector_embeddings.event_id: {e}")))?;
            let dims: i32 = row
                .try_get("dimensions")
                .map_err(|e| StoreError::Decode(format!("vector_embeddings.dimensions: {e}")))?;
            let bytes: Vec<u8> = row
                .try_get("vector")
                .map_err(|e| StoreError::Decode(format!("vector_embeddings.vector: {e}")))?;

            let dims_us = dims as usize;
            if dims_us != query.len() {
                continue; // dimension mismatch — silently skip, matching SQLite
            }
            if bytes.len() != dims_us * 4 {
                continue;
            }
            let mut v = Vec::with_capacity(dims_us);
            for chunk in bytes.chunks_exact(4) {
                let arr = [chunk[0], chunk[1], chunk[2], chunk[3]];
                v.push(f32::from_le_bytes(arr));
            }
            let score = cosine_distance(query, &v);
            scored.push(VectorSearchResult {
                event_id: event_id.to_string(),
                score,
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

/// Cosine distance: 1 - cos(θ). Matches SQLite reference impl.
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

#[cfg(test)]
mod tests {
    use super::cosine_distance;

    #[test]
    fn cosine_identical_vectors_are_zero() {
        let a = [1.0, 2.0, 3.0];
        let b = [1.0, 2.0, 3.0];
        assert!((cosine_distance(&a, &b)).abs() < 1e-6);
    }

    #[test]
    fn cosine_orthogonal_vectors_are_one() {
        let a = [1.0, 0.0];
        let b = [0.0, 1.0];
        assert!(((cosine_distance(&a, &b)) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_zero_vector_returns_one() {
        let a = [0.0, 0.0, 0.0];
        let b = [1.0, 1.0, 1.0];
        assert!(((cosine_distance(&a, &b)) - 1.0).abs() < 1e-6);
    }
}
