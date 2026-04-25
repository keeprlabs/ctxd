-- 0001_events.sql — events table, FTS column, indexes.
--
-- All statements are idempotent (gated on IF NOT EXISTS or DO blocks)
-- so the migration is safe to re-run at startup.
--
-- pg_trgm: required for the trigram GIN index on `subject` that
-- accelerates recursive `read --recursive` queries against
-- `subject LIKE '/prefix/%'`. If the role lacks CREATE EXTENSION
-- privilege, a DBA must run:
--
--     CREATE EXTENSION IF NOT EXISTS pg_trgm;
--
-- before pointing the daemon at this database. We attempt the
-- CREATE EXTENSION here as a convenience; the rest of the migration
-- still works (with a slower btree fallback) if pg_trgm is absent.

CREATE EXTENSION IF NOT EXISTS pg_trgm;

CREATE TABLE IF NOT EXISTS events (
    seq             BIGSERIAL PRIMARY KEY,
    id              UUID NOT NULL UNIQUE,
    source          TEXT NOT NULL,
    subject         TEXT NOT NULL,
    event_type      TEXT NOT NULL,
    time            TIMESTAMPTZ NOT NULL,
    datacontenttype TEXT NOT NULL DEFAULT 'application/json',
    data            JSONB NOT NULL,
    predecessorhash TEXT,
    signature       TEXT,
    parents         UUID[] NOT NULL DEFAULT '{}',
    attestation     BYTEA,
    specversion     TEXT NOT NULL DEFAULT '1.0',
    -- FTS column generated from data + subject + event_type. Stored
    -- (rather than expression-indexed) so reranking and snippets are
    -- cheap if we add them later. tsvector size grows ~30% over the
    -- raw text — acceptable.
    fts_tsv         TSVECTOR GENERATED ALWAYS AS (
        to_tsvector(
            'english',
            coalesce(data::text, '') || ' ' || subject || ' ' || event_type
        )
    ) STORED
);

CREATE INDEX IF NOT EXISTS idx_events_subject ON events (subject);
CREATE INDEX IF NOT EXISTS idx_events_time ON events (time);
CREATE INDEX IF NOT EXISTS idx_events_event_type ON events (event_type);

-- Trigram index on subject — accelerates `subject LIKE '/prefix/%'`
-- recursive reads. Falls back gracefully to the btree index above
-- when pg_trgm is unavailable; we don't error if this CREATE INDEX
-- fails because pg_trgm wasn't installed.
--
-- Notes:
--   * `pg_opclass` carries the operator-class definitions installed
--     by extensions. We check it (rather than `pg_extension`)
--     because `pg_extension` records the extension in its install
--     schema (often `public`), but the opclass lives in whichever
--     schema that extension was created in. Requiring the opclass
--     to be reachable via `search_path` is exactly the question we
--     care about.
--   * We qualify with `pg_catalog.gin_trgm_ops::text` to pin the
--     EXECUTE'd identifier to the extension's published name; if the
--     extension lives in a non-`public` schema the index creation
--     still succeeds because we resolve the opclass through the
--     search_path Postgres establishes for this connection.
DO $$
DECLARE
    opclass_schema TEXT;
BEGIN
    SELECT n.nspname INTO opclass_schema
    FROM pg_opclass oc
    JOIN pg_am am ON am.oid = oc.opcmethod
    JOIN pg_namespace n ON n.oid = oc.opcnamespace
    WHERE oc.opcname = 'gin_trgm_ops' AND am.amname = 'gin'
    LIMIT 1;
    IF opclass_schema IS NOT NULL THEN
        EXECUTE format(
            'CREATE INDEX IF NOT EXISTS idx_events_subject_trgm
             ON events USING gin (subject %I.gin_trgm_ops)',
            opclass_schema
        );
    END IF;
END
$$;

CREATE INDEX IF NOT EXISTS idx_events_fts ON events USING gin (fts_tsv);

-- Normalized side-table for parent edges. The `parents` array column
-- on `events` is canonical (sorted, deduplicated); this table exists
-- so "which events have parent X?" can use an index.
CREATE TABLE IF NOT EXISTS event_parents (
    event_id  UUID NOT NULL,
    parent_id UUID NOT NULL,
    PRIMARY KEY (event_id, parent_id)
);

CREATE INDEX IF NOT EXISTS idx_event_parents_parent ON event_parents (parent_id);
CREATE INDEX IF NOT EXISTS idx_event_parents_event ON event_parents (event_id);

-- KV view: latest value per subject under federation LWW (ADR 006).
CREATE TABLE IF NOT EXISTS kv_view (
    subject  TEXT PRIMARY KEY,
    event_id UUID NOT NULL,
    data     JSONB NOT NULL,
    time     TIMESTAMPTZ NOT NULL
);

-- Free-form daemon metadata (root cap key, etc.).
CREATE TABLE IF NOT EXISTS metadata (
    key   TEXT PRIMARY KEY,
    value BYTEA NOT NULL
);

-- Token revocation list.
CREATE TABLE IF NOT EXISTS revoked_tokens (
    token_id   TEXT PRIMARY KEY,
    revoked_at TIMESTAMPTZ NOT NULL
);
