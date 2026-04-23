//! Graph materialized view for entity-relationship data.
//!
//! Stores entities and relationships extracted from events in SQLite tables.
//! Supports queries by entity type, name pattern, and traversal of relationships.

use crate::store::StoreError;
use sqlx::sqlite::SqlitePool;

/// An entity in the graph.
pub struct Entity {
    /// Unique identifier for this entity.
    pub id: String,
    /// The type/category of this entity (e.g., "person", "file", "project").
    pub entity_type: String,
    /// Human-readable name.
    pub name: String,
    /// Arbitrary JSON properties.
    pub properties: serde_json::Value,
    /// The event ID this entity was extracted from.
    pub source_event_id: String,
}

/// A directed relationship between two entities.
pub struct Relationship {
    /// Unique identifier for this relationship.
    pub id: String,
    /// The source entity ID.
    pub from_entity_id: String,
    /// The target entity ID.
    pub to_entity_id: String,
    /// The type of relationship (e.g., "owns", "references", "depends_on").
    pub relationship_type: String,
    /// Arbitrary JSON properties.
    pub properties: serde_json::Value,
    /// The event ID this relationship was extracted from.
    pub source_event_id: String,
}

/// Graph view backed by SQLite tables in the same database as the event store.
#[derive(Clone)]
pub struct GraphView {
    pool: SqlitePool,
}

impl GraphView {
    /// Create a new graph view using the given connection pool.
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Add an entity to the graph.
    pub async fn add_entity(&self, entity: Entity) -> Result<(), StoreError> {
        let properties = serde_json::to_string(&entity.properties)?;
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query(
            r#"
            INSERT INTO graph_entities (id, entity_type, name, properties, source_event_id, created_at)
            VALUES (?, ?, ?, ?, ?, ?)
            ON CONFLICT(id) DO UPDATE SET
                entity_type = excluded.entity_type,
                name = excluded.name,
                properties = excluded.properties,
                source_event_id = excluded.source_event_id
            "#,
        )
        .bind(&entity.id)
        .bind(&entity.entity_type)
        .bind(&entity.name)
        .bind(&properties)
        .bind(&entity.source_event_id)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Add a relationship between two entities.
    pub async fn add_relationship(&self, rel: Relationship) -> Result<(), StoreError> {
        let properties = serde_json::to_string(&rel.properties)?;
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query(
            r#"
            INSERT INTO graph_relationships (id, from_entity_id, to_entity_id, relationship_type, properties, source_event_id, created_at)
            VALUES (?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(id) DO UPDATE SET
                from_entity_id = excluded.from_entity_id,
                to_entity_id = excluded.to_entity_id,
                relationship_type = excluded.relationship_type,
                properties = excluded.properties,
                source_event_id = excluded.source_event_id
            "#,
        )
        .bind(&rel.id)
        .bind(&rel.from_entity_id)
        .bind(&rel.to_entity_id)
        .bind(&rel.relationship_type)
        .bind(&properties)
        .bind(&rel.source_event_id)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Get entities, optionally filtered by type.
    pub async fn get_entities(&self, entity_type: Option<&str>) -> Result<Vec<Entity>, StoreError> {
        let rows: Vec<EntityRow> = if let Some(et) = entity_type {
            sqlx::query_as(
                "SELECT id, entity_type, name, properties, source_event_id FROM graph_entities WHERE entity_type = ? ORDER BY name",
            )
            .bind(et)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query_as(
                "SELECT id, entity_type, name, properties, source_event_id FROM graph_entities ORDER BY name",
            )
            .fetch_all(&self.pool)
            .await?
        };
        rows.into_iter().map(|r| r.into_entity()).collect()
    }

    /// Get a single entity by ID.
    pub async fn get_entity(&self, id: &str) -> Result<Option<Entity>, StoreError> {
        let row: Option<EntityRow> = sqlx::query_as(
            "SELECT id, entity_type, name, properties, source_event_id FROM graph_entities WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        match row {
            Some(r) => Ok(Some(r.into_entity()?)),
            None => Ok(None),
        }
    }

    /// Get entities related to the given entity, optionally filtered by relationship type.
    ///
    /// Returns relationships and the related entity (follows both outgoing and incoming edges).
    pub async fn get_related(
        &self,
        entity_id: &str,
        rel_type: Option<&str>,
    ) -> Result<Vec<(Relationship, Entity)>, StoreError> {
        // Outgoing relationships
        let outgoing: Vec<RelEntityRow> = if let Some(rt) = rel_type {
            sqlx::query_as(
                r#"
                SELECT r.id, r.from_entity_id, r.to_entity_id, r.relationship_type, r.properties, r.source_event_id,
                       e.id as eid, e.entity_type, e.name, e.properties as eprops, e.source_event_id as esrc
                FROM graph_relationships r
                JOIN graph_entities e ON e.id = r.to_entity_id
                WHERE r.from_entity_id = ? AND r.relationship_type = ?
                "#,
            )
            .bind(entity_id)
            .bind(rt)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query_as(
                r#"
                SELECT r.id, r.from_entity_id, r.to_entity_id, r.relationship_type, r.properties, r.source_event_id,
                       e.id as eid, e.entity_type, e.name, e.properties as eprops, e.source_event_id as esrc
                FROM graph_relationships r
                JOIN graph_entities e ON e.id = r.to_entity_id
                WHERE r.from_entity_id = ?
                "#,
            )
            .bind(entity_id)
            .fetch_all(&self.pool)
            .await?
        };

        // Incoming relationships
        let incoming: Vec<RelEntityRow> = if let Some(rt) = rel_type {
            sqlx::query_as(
                r#"
                SELECT r.id, r.from_entity_id, r.to_entity_id, r.relationship_type, r.properties, r.source_event_id,
                       e.id as eid, e.entity_type, e.name, e.properties as eprops, e.source_event_id as esrc
                FROM graph_relationships r
                JOIN graph_entities e ON e.id = r.from_entity_id
                WHERE r.to_entity_id = ? AND r.relationship_type = ?
                "#,
            )
            .bind(entity_id)
            .bind(rt)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query_as(
                r#"
                SELECT r.id, r.from_entity_id, r.to_entity_id, r.relationship_type, r.properties, r.source_event_id,
                       e.id as eid, e.entity_type, e.name, e.properties as eprops, e.source_event_id as esrc
                FROM graph_relationships r
                JOIN graph_entities e ON e.id = r.from_entity_id
                WHERE r.to_entity_id = ?
                "#,
            )
            .bind(entity_id)
            .fetch_all(&self.pool)
            .await?
        };

        let mut results = Vec::new();
        for row in outgoing.into_iter().chain(incoming) {
            results.push(row.into_rel_and_entity()?);
        }
        Ok(results)
    }

    /// Search entities by name pattern (SQL LIKE).
    pub async fn search_entities(&self, name_pattern: &str) -> Result<Vec<Entity>, StoreError> {
        let pattern = format!("%{name_pattern}%");
        let rows: Vec<EntityRow> = sqlx::query_as(
            "SELECT id, entity_type, name, properties, source_event_id FROM graph_entities WHERE name LIKE ? ORDER BY name",
        )
        .bind(&pattern)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(|r| r.into_entity()).collect()
    }
}

/// Internal row type for entity queries.
#[derive(sqlx::FromRow)]
struct EntityRow {
    id: String,
    entity_type: String,
    name: String,
    properties: String,
    source_event_id: String,
}

impl EntityRow {
    fn into_entity(self) -> Result<Entity, StoreError> {
        Ok(Entity {
            id: self.id,
            entity_type: self.entity_type,
            name: self.name,
            properties: serde_json::from_str(&self.properties)?,
            source_event_id: self.source_event_id,
        })
    }
}

/// Internal row type for relationship+entity join queries.
#[derive(sqlx::FromRow)]
struct RelEntityRow {
    id: String,
    from_entity_id: String,
    to_entity_id: String,
    relationship_type: String,
    properties: String,
    source_event_id: String,
    eid: String,
    entity_type: String,
    name: String,
    eprops: String,
    esrc: String,
}

impl RelEntityRow {
    fn into_rel_and_entity(self) -> Result<(Relationship, Entity), StoreError> {
        Ok((
            Relationship {
                id: self.id,
                from_entity_id: self.from_entity_id,
                to_entity_id: self.to_entity_id,
                relationship_type: self.relationship_type,
                properties: serde_json::from_str(&self.properties)?,
                source_event_id: self.source_event_id,
            },
            Entity {
                id: self.eid,
                entity_type: self.entity_type,
                name: self.name,
                properties: serde_json::from_str(&self.eprops)?,
                source_event_id: self.esrc,
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EventStore;

    async fn setup() -> (EventStore, GraphView) {
        let store = EventStore::open_memory().await.unwrap();
        let graph = store.graph_view();
        (store, graph)
    }

    #[tokio::test]
    async fn add_and_get_entity() {
        let (_store, graph) = setup().await;

        let entity = Entity {
            id: "e1".to_string(),
            entity_type: "person".to_string(),
            name: "Alice".to_string(),
            properties: serde_json::json!({"role": "engineer"}),
            source_event_id: "evt-1".to_string(),
        };
        graph.add_entity(entity).await.unwrap();

        let retrieved = graph.get_entity("e1").await.unwrap().unwrap();
        assert_eq!(retrieved.id, "e1");
        assert_eq!(retrieved.entity_type, "person");
        assert_eq!(retrieved.name, "Alice");
        assert_eq!(
            retrieved.properties,
            serde_json::json!({"role": "engineer"})
        );
    }

    #[tokio::test]
    async fn add_entities_and_relationship_then_query_related() {
        let (_store, graph) = setup().await;

        let alice = Entity {
            id: "e1".to_string(),
            entity_type: "person".to_string(),
            name: "Alice".to_string(),
            properties: serde_json::json!({}),
            source_event_id: "evt-1".to_string(),
        };
        let bob = Entity {
            id: "e2".to_string(),
            entity_type: "person".to_string(),
            name: "Bob".to_string(),
            properties: serde_json::json!({}),
            source_event_id: "evt-1".to_string(),
        };
        graph.add_entity(alice).await.unwrap();
        graph.add_entity(bob).await.unwrap();

        let rel = Relationship {
            id: "r1".to_string(),
            from_entity_id: "e1".to_string(),
            to_entity_id: "e2".to_string(),
            relationship_type: "knows".to_string(),
            properties: serde_json::json!({"since": 2020}),
            source_event_id: "evt-2".to_string(),
        };
        graph.add_relationship(rel).await.unwrap();

        // Query from Alice's perspective
        let related = graph.get_related("e1", None).await.unwrap();
        assert_eq!(related.len(), 1);
        assert_eq!(related[0].0.relationship_type, "knows");
        assert_eq!(related[0].1.name, "Bob");

        // Query from Bob's perspective (incoming edge)
        let related = graph.get_related("e2", None).await.unwrap();
        assert_eq!(related.len(), 1);
        assert_eq!(related[0].1.name, "Alice");

        // Filter by relationship type
        let related = graph.get_related("e1", Some("knows")).await.unwrap();
        assert_eq!(related.len(), 1);
        let related = graph.get_related("e1", Some("employs")).await.unwrap();
        assert!(related.is_empty());
    }

    #[tokio::test]
    async fn search_entities_by_name() {
        let (_store, graph) = setup().await;

        for (id, name) in &[
            ("e1", "Alice Smith"),
            ("e2", "Bob Jones"),
            ("e3", "Charlie Smith"),
        ] {
            let entity = Entity {
                id: id.to_string(),
                entity_type: "person".to_string(),
                name: name.to_string(),
                properties: serde_json::json!({}),
                source_event_id: "evt-1".to_string(),
            };
            graph.add_entity(entity).await.unwrap();
        }

        let results = graph.search_entities("Smith").await.unwrap();
        assert_eq!(results.len(), 2);

        let results = graph.search_entities("Bob").await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "Bob Jones");
    }

    #[tokio::test]
    async fn get_entities_filtered_by_type() {
        let (_store, graph) = setup().await;

        let person = Entity {
            id: "e1".to_string(),
            entity_type: "person".to_string(),
            name: "Alice".to_string(),
            properties: serde_json::json!({}),
            source_event_id: "evt-1".to_string(),
        };
        let project = Entity {
            id: "e2".to_string(),
            entity_type: "project".to_string(),
            name: "ctxd".to_string(),
            properties: serde_json::json!({}),
            source_event_id: "evt-1".to_string(),
        };
        graph.add_entity(person).await.unwrap();
        graph.add_entity(project).await.unwrap();

        let all = graph.get_entities(None).await.unwrap();
        assert_eq!(all.len(), 2);

        let people = graph.get_entities(Some("person")).await.unwrap();
        assert_eq!(people.len(), 1);
        assert_eq!(people[0].name, "Alice");

        let projects = graph.get_entities(Some("project")).await.unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].name, "ctxd");
    }
}
