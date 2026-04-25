-- 0004_vector.sql — raw vector embeddings for HNSW rebuild.
--
-- v0.3 keeps the in-process HNSW (instant-distance) used by SQLite
-- to keep behavior identical across backends. Vectors are stored as
-- raw little-endian f32 BYTEA so a future pgvector migration can
-- upgrade in place.

CREATE TABLE IF NOT EXISTS vector_embeddings (
    event_id   UUID PRIMARY KEY,
    model      TEXT NOT NULL,
    dimensions INTEGER NOT NULL,
    vector     BYTEA NOT NULL,
    created_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_vectors_model ON vector_embeddings (model);
