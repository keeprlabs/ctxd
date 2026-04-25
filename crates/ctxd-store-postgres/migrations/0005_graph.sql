-- 0005_graph.sql — entity-relationship view.

CREATE TABLE IF NOT EXISTS graph_entities (
    id               TEXT PRIMARY KEY,
    entity_type      TEXT NOT NULL,
    name             TEXT NOT NULL,
    properties       JSONB NOT NULL DEFAULT '{}'::jsonb,
    source_event_id  TEXT NOT NULL,
    created_at       TIMESTAMPTZ NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_entities_type ON graph_entities (entity_type);
CREATE INDEX IF NOT EXISTS idx_entities_name ON graph_entities (name);

CREATE TABLE IF NOT EXISTS graph_relationships (
    id                 TEXT PRIMARY KEY,
    from_entity_id     TEXT NOT NULL,
    to_entity_id       TEXT NOT NULL,
    relationship_type  TEXT NOT NULL,
    properties         JSONB NOT NULL DEFAULT '{}'::jsonb,
    source_event_id    TEXT NOT NULL,
    created_at         TIMESTAMPTZ NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_rel_from ON graph_relationships (from_entity_id);
CREATE INDEX IF NOT EXISTS idx_rel_to ON graph_relationships (to_entity_id);
CREATE INDEX IF NOT EXISTS idx_rel_type ON graph_relationships (relationship_type);
