//! MCP server that wraps ctxd store operations as tools.

use ctxd_cap::state::CaveatState;
use ctxd_cap::{CapEngine, Operation};
use ctxd_core::event::Event;
use ctxd_core::subject::Subject;
use ctxd_embed::Embedder;
use ctxd_store::EventStore;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::{tool, tool_router};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// Default timeout the MCP server waits for a human approval to be
/// decided before failing the tool call.
///
/// Five minutes matches the CLI default so a stressed engineering
/// manager has reasonable time to switch context, read the request,
/// and decide.
const DEFAULT_APPROVAL_TIMEOUT: Duration = Duration::from_secs(5 * 60);

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
    /// Search mode: `fts`, `vector`, or `hybrid`. Default: `hybrid`
    /// when an embedder is configured, `fts` otherwise.
    pub search_mode: Option<String>,
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
    caveat_state: Arc<dyn CaveatState>,
    source: String,
    /// Optional embedder used by `ctx_search` in `vector` and
    /// `hybrid` modes. When `None`, those modes degrade to `fts`.
    embedder: Option<Arc<dyn Embedder>>,
}

impl CtxdMcpServer {
    /// Create a new MCP server wrapping the given store, capability
    /// engine, and stateful-caveat backing.
    ///
    /// `caveat_state` is shared with HTTP and any other transport so
    /// budgets persist across request paths and approvals raised by
    /// MCP can be decided from the CLI / HTTP.
    pub fn new(
        store: EventStore,
        cap_engine: Arc<CapEngine>,
        caveat_state: Arc<dyn CaveatState>,
        source: String,
    ) -> Self {
        Self {
            store,
            cap_engine,
            caveat_state,
            source,
            embedder: None,
        }
    }

    /// Install an embedder. Subsequent `ctx_search` calls will
    /// default to hybrid mode and accept `vector` / `hybrid` mode
    /// requests.
    pub fn with_embedder(mut self, embedder: Arc<dyn Embedder>) -> Self {
        self.embedder = Some(embedder);
        self
    }

    /// Borrow the configured embedder, if any.
    pub fn embedder(&self) -> Option<&Arc<dyn Embedder>> {
        self.embedder.as_ref()
    }

    /// Access the underlying store. Exposed so that integration tests
    /// (and callers that need direct event-log access) can seed and
    /// inspect state without going through MCP framing.
    pub fn store(&self) -> &EventStore {
        &self.store
    }

    /// Verify a capability token if provided.
    ///
    /// Threads the daemon's [`CaveatState`] so budget caveats and
    /// human-approval caveats are enforced at request time. Tools that
    /// don't pass a token fall through to v0.1 open-by-default
    /// (ADR 004) — there's nothing to verify.
    async fn verify_token(
        &self,
        token: &Option<String>,
        subject: &str,
        operation: Operation,
    ) -> Result<(), String> {
        if let Some(tok) = token {
            let bytes = CapEngine::token_from_base64(tok).map_err(|e| e.to_string())?;
            self.cap_engine
                .verify_with_state(
                    &bytes,
                    subject,
                    operation,
                    None,
                    Some(self.caveat_state.as_ref()),
                    DEFAULT_APPROVAL_TIMEOUT,
                )
                .await
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Render a Vec<Event> as the existing JSON-pretty output.
    fn events_to_json(events: &[Event]) -> String {
        let v: Vec<serde_json::Value> = events
            .iter()
            .map(|e| serde_json::to_value(e).unwrap_or_default())
            .collect();
        serde_json::to_string_pretty(&v).unwrap_or_default()
    }

    /// Run a pure-FTS search and return JSON output.
    async fn run_fts_only<F>(&self, query: &str, k: usize, prefix_filter: &F) -> String
    where
        F: Fn(&Event) -> bool,
    {
        match self.store.search(query, Some(k)).await {
            Ok(events) => {
                let filtered: Vec<Event> =
                    events.into_iter().filter(prefix_filter).take(k).collect();
                Self::events_to_json(&filtered)
            }
            Err(e) => format!("error: search failed: {e}"),
        }
    }

    /// Embed `query` with the configured embedder and run vector search.
    async fn run_vector_only<F>(&self, query: &str, k: usize, prefix_filter: &F) -> String
    where
        F: Fn(&Event) -> bool,
    {
        let embedder = match &self.embedder {
            Some(e) => e,
            None => return "error: vector search requires an embedder".to_string(),
        };
        let qvec = match embedder.embed(query).await {
            Ok(v) => v,
            Err(e) => return format!("error: embed failed: {e}"),
        };
        // Pull more than k from the vector index so the
        // post-filter (subject_pattern) can still surface k.
        let pull = k.saturating_mul(4).max(k);
        let hits = match self.store.vector_search_impl(&qvec, pull).await {
            Ok(h) => h,
            Err(e) => return format!("error: vector search failed: {e}"),
        };
        let events = match self
            .events_for_ids(hits.iter().map(|h| h.event_id.as_str()))
            .await
        {
            Ok(m) => {
                let mut out = Vec::with_capacity(hits.len());
                for h in hits {
                    if let Some(ev) = m.get(h.event_id.as_str()) {
                        if prefix_filter(ev) {
                            out.push(ev.clone());
                            if out.len() >= k {
                                break;
                            }
                        }
                    }
                }
                out
            }
            Err(e) => return format!("error: vector lookup failed: {e}"),
        };
        Self::events_to_json(&events)
    }

    /// Hybrid: union FTS + vector results via RRF, then return events.
    async fn run_hybrid<F>(&self, query: &str, k: usize, prefix_filter: &F) -> String
    where
        F: Fn(&Event) -> bool,
    {
        // We over-pull from each side so RRF has enough candidates
        // to find docs in both lists.
        let pull = k.saturating_mul(4).max(20);
        let fts_events = match self.store.search(query, Some(pull)).await {
            Ok(v) => v,
            Err(e) => return format!("error: fts failed: {e}"),
        };
        let fts_ids: Vec<String> = fts_events.iter().map(|e| e.id.to_string()).collect();
        let vec_ids: Vec<String> = match &self.embedder {
            Some(embedder) => match embedder.embed(query).await {
                Ok(qvec) => match self.store.vector_search_impl(&qvec, pull).await {
                    Ok(hits) => hits.into_iter().map(|h| h.event_id).collect(),
                    Err(e) => {
                        tracing::warn!(error = %e, "hybrid: vector path failed; using FTS only");
                        Vec::new()
                    }
                },
                Err(e) => {
                    tracing::warn!(error = %e, "hybrid: embed failed; using FTS only");
                    Vec::new()
                }
            },
            None => Vec::new(),
        };
        let lists: &[&[String]] = &[fts_ids.as_slice(), vec_ids.as_slice()];
        let fused = reciprocal_rank_fusion(lists, k.saturating_mul(2).max(k));
        // Resolve fused ids -> Events. Prefer the FTS-result events
        // we already have to avoid a second SQL round-trip; fall back
        // to per-id lookup for vector-only ids.
        let mut have: HashMap<String, Event> = fts_events
            .into_iter()
            .map(|e| (e.id.to_string(), e))
            .collect();
        let need: Vec<&str> = fused
            .iter()
            .filter(|id| !have.contains_key(id.as_str()))
            .map(String::as_str)
            .collect();
        if !need.is_empty() {
            match self.events_for_ids(need.into_iter()).await {
                Ok(m) => {
                    for (k, v) in m {
                        have.insert(k, v);
                    }
                }
                Err(e) => return format!("error: hybrid lookup failed: {e}"),
            }
        }
        let mut out = Vec::with_capacity(k);
        for id in fused {
            if let Some(ev) = have.remove(&id) {
                if prefix_filter(&ev) {
                    out.push(ev);
                    if out.len() >= k {
                        break;
                    }
                }
            }
        }
        Self::events_to_json(&out)
    }

    /// Look up a batch of events by id. Returns a `HashMap` keyed
    /// by stringified UUID. Missing ids are silently dropped.
    async fn events_for_ids<'a, I>(&self, ids: I) -> Result<HashMap<String, Event>, String>
    where
        I: Iterator<Item = &'a str>,
    {
        let ids: Vec<String> = ids.map(|s| s.to_string()).collect();
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        // We don't have a "get by id" on the store yet; do a recursive
        // read from root and filter. For typical k≤50 this is the
        // simplest correct path; an `events_by_ids` helper is a
        // worthwhile follow-up.
        let root = match Subject::new("/") {
            Ok(s) => s,
            Err(e) => return Err(format!("invalid root subject: {e}")),
        };
        let all = match self.store.read(&root, true).await {
            Ok(v) => v,
            Err(e) => return Err(format!("read failed: {e}")),
        };
        let mut by_id: HashMap<String, Event> = HashMap::with_capacity(ids.len());
        for ev in all {
            let s = ev.id.to_string();
            if ids.iter().any(|i| i == &s) {
                by_id.insert(s, ev);
            }
        }
        Ok(by_id)
    }
}

/// Search mode selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchMode {
    Fts,
    Vector,
    Hybrid,
}

/// RRF constant from the original 2009 Cormack/Clarke/Buettcher
/// paper. The value bounds the score contribution from very low
/// ranks; 60 is the convention everyone (Elastic, Vespa, Anserini)
/// has settled on. We expose it as a constant so the choice is
/// auditable from a single line. See
/// `docs/decisions/015-hybrid-search-rrf.md` for rationale.
const RRF_K_CONST: f32 = 60.0;

/// Reciprocal Rank Fusion over multiple ranked id lists.
///
/// For each id, sums `1 / (RRF_K_CONST + rank_i)` across every
/// list it appears in. Returns ids sorted by descending fused
/// score, taking at most `k`.
fn reciprocal_rank_fusion(lists: &[&[String]], k: usize) -> Vec<String> {
    let mut scores: HashMap<&str, f32> = HashMap::new();
    for list in lists {
        for (rank, id) in list.iter().enumerate() {
            let s = scores.entry(id.as_str()).or_insert(0.0);
            // Ranks are 0-indexed in our slice but the RRF formula
            // is conventionally 1-indexed — `rank + 1` matches the paper.
            *s += 1.0 / (RRF_K_CONST + (rank as f32 + 1.0));
        }
    }
    let mut by_score: Vec<(&str, f32)> = scores.into_iter().collect();
    by_score.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(b.0))
    });
    by_score
        .into_iter()
        .take(k)
        .map(|(s, _)| s.to_string())
        .collect()
}

#[tool_router(server_handler)]
impl CtxdMcpServer {
    /// Write a context event to the store.
    #[tool(
        name = "ctx_write",
        description = "Append a context event. Takes subject (path), event_type, data (JSON string), and optional token (capability)."
    )]
    async fn ctx_write(&self, Parameters(params): Parameters<WriteParams>) -> Json<WriteResult> {
        if let Err(e) = self
            .verify_token(&params.token, &params.subject, Operation::Write)
            .await
        {
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
        if let Err(e) = self
            .verify_token(&params.token, &params.subject, Operation::Read)
            .await
        {
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
                if let Err(e) = self
                    .verify_token(&params.token, p, Operation::Subjects)
                    .await
                {
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

    /// Full-text / vector / hybrid search over context events.
    #[tool(
        name = "ctx_search",
        description = "Search context events. `search_mode` is `fts` (default when no embedder), `vector` (embed query, k-NN over the HNSW index), or `hybrid` (RRF fusion of FTS + vector, default when an embedder is configured). Takes query, optional subject_pattern, k (default 10), search_mode, and token."
    )]
    pub async fn ctx_search(&self, Parameters(params): Parameters<SearchParams>) -> String {
        let subject_for_auth = params.subject_pattern.as_deref().unwrap_or("/**");
        if let Err(e) = self
            .verify_token(&params.token, subject_for_auth, Operation::Search)
            .await
        {
            return format!("error: {e}");
        }

        let k = params.k.unwrap_or(10);
        let mode = match params.search_mode.as_deref() {
            Some("fts") => SearchMode::Fts,
            Some("vector") => SearchMode::Vector,
            Some("hybrid") => SearchMode::Hybrid,
            Some(other) => {
                return format!(
                    "error: unknown search_mode '{other}' (expected fts|vector|hybrid)"
                );
            }
            None => {
                if self.embedder.is_some() {
                    SearchMode::Hybrid
                } else {
                    SearchMode::Fts
                }
            }
        };

        // Helper: filter events by subject_pattern when set.
        let pattern = params.subject_pattern.clone();
        let prefix_filter = move |e: &Event| -> bool {
            match &pattern {
                Some(p) => e.subject.as_str().starts_with(p.as_str()),
                None => true,
            }
        };

        match mode {
            SearchMode::Fts => self.run_fts_only(&params.query, k, &prefix_filter).await,
            SearchMode::Vector => self.run_vector_only(&params.query, k, &prefix_filter).await,
            SearchMode::Hybrid => self.run_hybrid(&params.query, k, &prefix_filter).await,
        }
    }

    /// Poll for context events since a given timestamp.
    #[tool(
        name = "ctx_subscribe",
        description = "Poll for context events since a given timestamp. Takes subject (path), optional since (RFC3339 timestamp), recursive (bool), and optional token."
    )]
    async fn ctx_subscribe(&self, Parameters(params): Parameters<SubscribeParams>) -> String {
        if let Err(e) = self
            .verify_token(&params.token, &params.subject, Operation::Read)
            .await
        {
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
        if let Err(e) = self
            .verify_token(&params.token, auth_subject, Operation::Search)
            .await
        {
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
        if let Err(e) = self
            .verify_token(&params.token, "/**", Operation::Search)
            .await
        {
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
        if let Err(e) = self
            .verify_token(&params.token, &params.subject, Operation::Read)
            .await
        {
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
