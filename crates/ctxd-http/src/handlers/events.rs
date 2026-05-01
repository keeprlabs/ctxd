//! `GET /v1/events`, `GET /v1/events/:id`, `GET /v1/events/stream`.
//!
//! The `/v1/events` list endpoint uses opaque cursor pagination
//! (encoded `seq`), `/v1/events/:id` is a point lookup, and
//! `/v1/events/stream` fans live appends out as Server-Sent Events.

use crate::responses::EventsCursor;
use crate::router::AppState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::Json;
use ctxd_core::event::Event;
use ctxd_core::subject::Subject;
use futures::Stream;
use serde::Deserialize;
use std::convert::Infallible;
use std::time::Duration;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

/// Default `limit` for `/v1/events` when the query string omits it.
const DEFAULT_LIMIT: usize = 50;
/// Hard cap on `limit` so a client can't ask for the entire log.
const MAX_LIMIT: usize = 500;

/// Query params for `GET /v1/events`.
#[derive(Debug, Deserialize)]
pub(crate) struct ListQuery {
    /// Optional subject filter. When set, recursive descent is implied
    /// (descendants under this subject are included).
    subject: Option<String>,
    /// Cursor from a prior page's `next_cursor`.
    before: Option<String>,
    /// Page size. Defaults to 50, capped at 500.
    limit: Option<usize>,
}

/// `GET /v1/events?subject=&before=&limit=` — list events newest-first.
#[tracing::instrument(skip(state))]
pub(crate) async fn list_events(
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);

    let before_seq = match q.before.as_deref() {
        Some(s) => Some(
            EventsCursor::decode(s)
                .map_err(|_| (StatusCode::BAD_REQUEST, "invalid cursor".to_string()))?
                .seq,
        ),
        None => None,
    };

    let subject_owned: Option<Subject> = match q.subject.as_deref() {
        Some(s) => Some(
            Subject::new(s)
                .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid subject: {e}")))?,
        ),
        None => None,
    };

    let rows = state
        .store
        .read_paginated(subject_owned.as_ref(), before_seq, limit, true)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let next_cursor = rows
        .last()
        .map(|(seq, _)| EventsCursor { seq: *seq }.encode());
    let events: Vec<&Event> = rows.iter().map(|(_, e)| e).collect();
    Ok(Json(serde_json::json!({
        "events": events,
        "next_cursor": next_cursor,
    })))
}

/// `GET /v1/events/:id` — single event detail.
#[tracing::instrument(skip(state))]
pub(crate) async fn event_by_id(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Event>, (StatusCode, String)> {
    let id = uuid::Uuid::parse_str(&id)
        .map_err(|_| (StatusCode::BAD_REQUEST, "invalid event id".to_string()))?;
    match state.store.event_by_id(id).await {
        Ok(Some(e)) => Ok(Json(e)),
        Ok(None) => Err((StatusCode::NOT_FOUND, "no such event".to_string())),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    }
}

/// `GET /v1/events/stream` — Server-Sent Events live tail.
///
/// On connect, the client receives no historical replay (it already
/// loaded a recent slice via `/v1/events`). Every successful append
/// thereafter fans out as a `data: <event-json>\n\n` frame. Lagged
/// consumers (256-slot broadcast buffer) get a single `event: lagged`
/// frame as a hint to re-fetch a snapshot.
///
/// A 15-second keep-alive ping prevents intermediate proxies from
/// timing out idle connections during quiet periods.
pub(crate) async fn stream_events(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    let rx = state.store.subscribe(None);
    let stream = BroadcastStream::new(rx).map(|item| {
        // Always Ok(...) — convert errors into protocol-level frames so
        // the connection can keep going. Infallible is required by axum's
        // Sse Stream signature.
        let frame = match item {
            Ok(ev) => SseEvent::default()
                .event("event")
                .json_data(&ev)
                .unwrap_or_else(|_| SseEvent::default().event("error")),
            Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(n)) => {
                SseEvent::default().event("lagged").data(n.to_string())
            }
        };
        Ok::<_, Infallible>(frame)
    });
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}
