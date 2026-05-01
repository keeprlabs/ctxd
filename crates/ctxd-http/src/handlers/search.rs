//! `GET /v1/search` — FTS5 search with snippets.

use crate::router::AppState;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use ctxd_core::event::Event;
use serde::{Deserialize, Serialize};
use std::time::Instant;

const DEFAULT_K: usize = 50;
const MAX_K: usize = 200;

#[derive(Debug, Deserialize)]
pub(crate) struct SearchQuery {
    q: Option<String>,
    k: Option<usize>,
}

#[derive(Debug, Serialize)]
pub(crate) struct SearchResult {
    #[serde(flatten)]
    pub event: Event,
    pub snippet: String,
    pub rank: f32,
}

#[derive(Debug, Serialize)]
pub(crate) struct SearchResponse {
    pub query: String,
    pub results: Vec<SearchResult>,
    pub took_ms: u64,
}

/// `GET /v1/search?q=...&k=N` — FTS5 search ordered by BM25.
#[tracing::instrument(skip(state))]
pub(crate) async fn search(
    State(state): State<AppState>,
    Query(q): Query<SearchQuery>,
) -> Result<Json<SearchResponse>, (StatusCode, String)> {
    let query =
        q.q.as_deref()
            .ok_or((StatusCode::BAD_REQUEST, "missing q parameter".to_string()))?;
    if query.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "q must not be empty".to_string()));
    }
    let k = q.k.unwrap_or(DEFAULT_K).min(MAX_K).max(1);

    let started = Instant::now();
    let hits = state
        .store
        .search_with_snippets(query, k)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let took_ms = started.elapsed().as_millis() as u64;

    Ok(Json(SearchResponse {
        query: query.to_string(),
        results: hits
            .into_iter()
            .map(|h| SearchResult {
                event: h.event,
                snippet: h.snippet,
                rank: h.rank,
            })
            .collect(),
        took_ms,
    }))
}
