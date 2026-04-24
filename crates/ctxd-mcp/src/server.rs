//! MCP server that wraps ctxd store operations as tools.

use ctxd_cap::{CapEngine, Operation};
use ctxd_core::event::Event;
use ctxd_core::subject::Subject;
use ctxd_store::EventStore;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::{tool, tool_router};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Arguments for ctx_write tool.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct WriteParams {
    /// Subject path for the event.
    pub subject: String,
    /// Event type descriptor.
    pub event_type: String,
    /// Event data as JSON string.
    pub data: String,
    /// Optional capability token (base64-encoded).
    pub token: Option<String>,
}

/// Result of ctx_write.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct WriteResult {
    /// The event ID.
    pub id: String,
    /// The subject path.
    pub subject: String,
    /// The predecessor hash, if any.
    pub predecessorhash: Option<String>,
}

/// Arguments for ctx_read tool.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReadParams {
    /// Subject path to read from.
    pub subject: String,
    /// Whether to read recursively.
    #[serde(default)]
    pub recursive: bool,
    /// Optional capability token (base64-encoded).
    pub token: Option<String>,
}

/// Arguments for ctx_subjects tool.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SubjectsParams {
    /// Optional prefix to filter subjects.
    pub prefix: Option<String>,
    /// Whether to list recursively.
    #[serde(default)]
    pub recursive: bool,
    /// Optional capability token (base64-encoded).
    pub token: Option<String>,
}

/// Arguments for ctx_search tool.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchParams {
    /// Full-text search query.
    pub query: String,
    /// Optional subject prefix filter.
    pub subject_pattern: Option<String>,
    /// Maximum number of results (default 10).
    pub k: Option<usize>,
    /// Optional capability token (base64-encoded).
    pub token: Option<String>,
}

/// Arguments for the `ctx_entities` tool.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct EntitiesParams {
    /// Filter by entity type.
    pub entity_type: Option<String>,
    /// Filter by entity name substring (SQL LIKE pattern on `name`).
    pub name_pattern: Option<String>,
    /// Optional subject prefix to scope which source events are considered.
    ///
    /// NOTE: today this is a post-filter on `source_event_id` — entities are
    /// already materialized in `graph_entities` so we filter after the fact.
    /// When we re-derive the graph view per-query this will tighten up.
    pub subject_pattern: Option<String>,
    /// Maximum number of entities to return.
    pub limit: Option<usize>,
    /// Optional capability token (base64-encoded).
    pub token: Option<String>,
}

/// Arguments for the `ctx_related` tool.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RelatedParams {
    /// The entity to find relationships for.
    pub entity_id: String,
    /// Filter by relationship type (e.g. "authored").
    pub relationship_type: Option<String>,
    /// Optional capability token (base64-encoded).
    pub token: Option<String>,
}

/// Arguments for the `ctx_timeline` tool.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TimelineParams {
    /// Subject path to read at a point in time.
    pub subject: String,
    /// RFC3339 timestamp. Returns state as of this time (`time <= as_of`).
    pub as_of: String,
    /// Include descendant subjects.
    #[serde(default)]
    pub recursive: bool,
    /// Optional capability token (base64-encoded).
    pub token: Option<String>,
}

/// Arguments for ctx_subscribe tool.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SubscribeParams {
    /// Subject path to poll events from.
    pub subject: String,
    /// Optional RFC3339 timestamp; only return events after this time.
    pub since: Option<String>,
    /// Whether to include child subjects.
    #[serde(default)]
    pub recursive: bool,
    /// Optional capability token (base64-encoded).
    pub token: Option<String>,
}

/// The ctxd MCP server.
#[derive(Clone)]
pub struct CtxdMcpServer {
    store: EventStore,
    cap_engine: Arc<CapEngine>,
    source: String,
}

impl CtxdMcpServer {
    /// Create a new MCP server wrapping the given store and capability engine.
    pub fn new(store: EventStore, cap_engine: Arc<CapEngine>, source: String) -> Self {
        Self {
            store,
            cap_engine,
            source,
        }
    }

    /// Access the underlying store. Exposed so that integration tests
    /// (and callers that need direct event-log access) can seed and
    /// inspect state without going through MCP framing.
    pub fn store(&self) -> &EventStore {
        &self.store
    }

    /// Verify a capability token if provided.
    fn verify_token(
        &self,
        token: &Option<String>,
        subject: &str,
        operation: Operation,
    ) -> Result<(), String> {
        if let Some(tok) = token {
            let bytes = CapEngine::token_from_base64(tok).map_err(|e| e.to_string())?;
            self.cap_engine
                .verify(&bytes, subject, operation, None)
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

#[tool_router(server_handler)]
impl CtxdMcpServer {
    /// Write a context event to the store.
    #[tool(
        name = "ctx_write",
        description = "Append a context event. Takes subject (path), event_type, data (JSON string), and optional token (capability)."
    )]
    async fn ctx_write(&self, Parameters(params): Parameters<WriteParams>) -> Json<WriteResult> {
        if let Err(e) = self.verify_token(&params.token, &params.subject, Operation::Write) {
            return Json(WriteResult {
                id: String::new(),
                subject: format!("error: {e}"),
                predecessorhash: None,
            });
        }

        let subject = match Subject::new(&params.subject) {
            Ok(s) => s,
            Err(e) => {
                return Json(WriteResult {
                    id: String::new(),
                    subject: format!("error: invalid subject: {e}"),
                    predecessorhash: None,
                });
            }
        };

        let data: serde_json::Value = match serde_json::from_str(&params.data) {
            Ok(d) => d,
            Err(e) => {
                return Json(WriteResult {
                    id: String::new(),
                    subject: format!("error: invalid JSON data: {e}"),
                    predecessorhash: None,
                });
            }
        };

        let event = Event::new(self.source.clone(), subject, params.event_type, data);

        match self.store.append(event).await {
            Ok(stored) => Json(WriteResult {
                id: stored.id.to_string(),
                subject: stored.subject.as_str().to_string(),
                predecessorhash: stored.predecessorhash,
            }),
            Err(e) => Json(WriteResult {
                id: String::new(),
                subject: format!("error: write failed: {e}"),
                predecessorhash: None,
            }),
        }
    }

    /// Read context events for a subject.
    #[tool(
        name = "ctx_read",
        description = "Read context events for a subject path. Takes subject (path), recursive (bool), and optional token."
    )]
    async fn ctx_read(&self, Parameters(params): Parameters<ReadParams>) -> String {
        if let Err(e) = self.verify_token(&params.token, &params.subject, Operation::Read) {
            return format!("error: {e}");
        }

        let subject = match Subject::new(&params.subject) {
            Ok(s) => s,
            Err(e) => return format!("error: invalid subject: {e}"),
        };

        match self.store.read(&subject, params.recursive).await {
            Ok(events) => {
                let output: Vec<serde_json::Value> = events
                    .iter()
                    .map(|e| serde_json::to_value(e).unwrap_or_default())
                    .collect();
                serde_json::to_string_pretty(&output).unwrap_or_default()
            }
            Err(e) => format!("error: read failed: {e}"),
        }
    }

    /// List context subjects.
    #[tool(
        name = "ctx_subjects",
        description = "List context subjects. Takes optional prefix (path), recursive (bool), and optional token."
    )]
    async fn ctx_subjects(&self, Parameters(params): Parameters<SubjectsParams>) -> String {
        let prefix = match &params.prefix {
            Some(p) => {
                if let Err(e) = self.verify_token(&params.token, p, Operation::Subjects) {
                    return format!("error: {e}");
                }
                match Subject::new(p) {
                    Ok(s) => Some(s),
                    Err(e) => return format!("error: invalid prefix: {e}"),
                }
            }
            None => None,
        };

        match self.store.subjects(prefix.as_ref(), params.recursive).await {
            Ok(subjects) => serde_json::to_string_pretty(&subjects).unwrap_or_default(),
            Err(e) => format!("error: subjects failed: {e}"),
        }
    }

    /// Full-text search over context events.
    #[tool(
        name = "ctx_search",
        description = "Full-text search over context events. Takes query (string), optional subject_pattern (prefix filter), optional k (max results, default 10), and optional token."
    )]
    async fn ctx_search(&self, Parameters(params): Parameters<SearchParams>) -> String {
        // Use subject_pattern for token verification if provided, otherwise use wildcard
        let subject_for_auth = params.subject_pattern.as_deref().unwrap_or("/**");
        if let Err(e) = self.verify_token(&params.token, subject_for_auth, Operation::Search) {
            return format!("error: {e}");
        }

        let k = params.k.unwrap_or(10);

        match self.store.search(&params.query, Some(k)).await {
            Ok(events) => {
                let filtered: Vec<&ctxd_core::event::Event> =
                    if let Some(ref pattern) = params.subject_pattern {
                        events
                            .iter()
                            .filter(|e| e.subject.as_str().starts_with(pattern.as_str()))
                            .take(k)
                            .collect()
                    } else {
                        events.iter().take(k).collect()
                    };
                let output: Vec<serde_json::Value> = filtered
                    .iter()
                    .map(|e| serde_json::to_value(e).unwrap_or_default())
                    .collect();
                serde_json::to_string_pretty(&output).unwrap_or_default()
            }
            Err(e) => format!("error: search failed: {e}"),
        }
    }

    /// Poll for context events since a given timestamp.
    #[tool(
        name = "ctx_subscribe",
        description = "Poll for context events since a given timestamp. Takes subject (path), optional since (RFC3339 timestamp), recursive (bool), and optional token."
    )]
    async fn ctx_subscribe(&self, Parameters(params): Parameters<SubscribeParams>) -> String {
        if let Err(e) = self.verify_token(&params.token, &params.subject, Operation::Read) {
            return format!("error: {e}");
        }

        let subject = match Subject::new(&params.subject) {
            Ok(s) => s,
            Err(e) => return format!("error: invalid subject: {e}"),
        };

        let since = match &params.since {
            Some(ts) => match chrono::DateTime::parse_from_rfc3339(ts) {
                Ok(dt) => dt.with_timezone(&chrono::Utc),
                Err(e) => return format!("error: invalid timestamp: {e}"),
            },
            None => chrono::DateTime::<chrono::Utc>::MIN_UTC,
        };

        match self
            .store
            .read_since(&subject, since, params.recursive)
            .await
        {
            Ok(events) => {
                let output: Vec<serde_json::Value> = events
                    .iter()
                    .map(|e| serde_json::to_value(e).unwrap_or_default())
                    .collect();
                serde_json::to_string_pretty(&output).unwrap_or_default()
            }
            Err(e) => format!("error: subscribe failed: {e}"),
        }
    }

    /// Query the graph view for entities.
    #[tool(
        name = "ctx_entities",
        description = "Query materialized graph entities by type and/or name pattern. Takes optional entity_type, name_pattern (substring), subject_pattern (filter by source event subject prefix), limit, and token."
    )]
    pub async fn ctx_entities(&self, Parameters(params): Parameters<EntitiesParams>) -> String {
        // Graph entities are scoped by their source event's subject. For
        // auth we check the widest subject the caller is asking about —
        // if they filtered by subject_pattern we use that, otherwise
        // require a cap covering `/**`.
        let auth_subject = params.subject_pattern.as_deref().unwrap_or("/**");
        if let Err(e) = self.verify_token(&params.token, auth_subject, Operation::Search) {
            return format!("error: {e}");
        }

        let graph = self.store.graph_view();
        let entities = match graph.get_entities(params.entity_type.as_deref()).await {
            Ok(v) => v,
            Err(e) => return format!("error: entities failed: {e}"),
        };

        // NOTE: `subject_pattern` is currently accepted for auth scoping
        // only. The graph row doesn't carry its source event's subject
        // yet, so we can't post-filter here — a follow-up will join
        // `graph_entities` against `events` to narrow by subject prefix.
        let mut filtered: Vec<serde_json::Value> = entities
            .into_iter()
            .filter(|e| match &params.name_pattern {
                Some(pat) => e.name.contains(pat),
                None => true,
            })
            .map(|e| {
                serde_json::json!({
                    "id": e.id,
                    "entity_type": e.entity_type,
                    "name": e.name,
                    "properties": e.properties,
                    "source_event_id": e.source_event_id,
                })
            })
            .collect();

        if let Some(limit) = params.limit {
            filtered.truncate(limit);
        }
        serde_json::to_string_pretty(&filtered).unwrap_or_default()
    }

    /// Query related entities via graph relationships.
    #[tool(
        name = "ctx_related",
        description = "Return relationships + the connected entity for a given entity_id. Optional relationship_type narrows by edge label. Optional capability token."
    )]
    pub async fn ctx_related(&self, Parameters(params): Parameters<RelatedParams>) -> String {
        // Without per-entity subject metadata on the graph row, we require
        // a cap for the wildcard subject to query relationships. Phase 2
        // can refine this once we carry the source subject on graph rows.
        if let Err(e) = self.verify_token(&params.token, "/**", Operation::Search) {
            return format!("error: {e}");
        }

        let graph = self.store.graph_view();
        match graph
            .get_related(&params.entity_id, params.relationship_type.as_deref())
            .await
        {
            Ok(pairs) => {
                let output: Vec<serde_json::Value> = pairs
                    .into_iter()
                    .map(|(rel, ent)| {
                        serde_json::json!({
                            "relationship": {
                                "id": rel.id,
                                "from_entity_id": rel.from_entity_id,
                                "to_entity_id": rel.to_entity_id,
                                "relationship_type": rel.relationship_type,
                                "properties": rel.properties,
                                "source_event_id": rel.source_event_id,
                            },
                            "entity": {
                                "id": ent.id,
                                "entity_type": ent.entity_type,
                                "name": ent.name,
                                "properties": ent.properties,
                                "source_event_id": ent.source_event_id,
                            }
                        })
                    })
                    .collect();
                serde_json::to_string_pretty(&output).unwrap_or_default()
            }
            Err(e) => format!("error: related failed: {e}"),
        }
    }

    /// Return events for a subject as of a given timestamp.
    #[tool(
        name = "ctx_timeline",
        description = "Temporal read: returns events with time <= as_of for the given subject (optionally recursive). as_of is RFC3339."
    )]
    pub async fn ctx_timeline(&self, Parameters(params): Parameters<TimelineParams>) -> String {
        if let Err(e) = self.verify_token(&params.token, &params.subject, Operation::Read) {
            return format!("error: {e}");
        }
        let subject = match Subject::new(&params.subject) {
            Ok(s) => s,
            Err(e) => return format!("error: invalid subject: {e}"),
        };
        let as_of = match chrono::DateTime::parse_from_rfc3339(&params.as_of) {
            Ok(dt) => dt.with_timezone(&chrono::Utc),
            Err(e) => return format!("error: invalid as_of timestamp: {e}"),
        };
        match self.store.read_at(&subject, as_of, params.recursive).await {
            Ok(events) => {
                let output: Vec<serde_json::Value> = events
                    .iter()
                    .map(|e| serde_json::to_value(e).unwrap_or_default())
                    .collect();
                serde_json::to_string_pretty(&output).unwrap_or_default()
            }
            Err(e) => format!("error: timeline failed: {e}"),
        }
    }
}
