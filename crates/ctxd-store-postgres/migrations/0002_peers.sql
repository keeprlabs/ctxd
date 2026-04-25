-- 0002_peers.sql — federation peers and replication cursors.

CREATE TABLE IF NOT EXISTS peers (
    peer_id          TEXT PRIMARY KEY,
    url              TEXT NOT NULL,
    public_key       BYTEA NOT NULL,
    granted_subjects JSONB NOT NULL DEFAULT '[]'::jsonb,
    trust_level      JSONB NOT NULL DEFAULT '{}'::jsonb,
    added_at         TIMESTAMPTZ NOT NULL
);

CREATE TABLE IF NOT EXISTS peer_cursors (
    peer_id         TEXT NOT NULL,
    subject_pattern TEXT NOT NULL,
    last_event_id   UUID,
    last_event_time TIMESTAMPTZ,
    updated_at      TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (peer_id, subject_pattern)
);
