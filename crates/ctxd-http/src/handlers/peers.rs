//! `GET /v1/peers` and `DELETE /v1/peers/:peer_id` — federation peer
//! list and removal. Both require an admin bearer token.

use crate::handlers::require_admin;
use crate::responses::{PeerListItem, PeerListResponse};
use crate::router::AppState;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;

/// `GET /v1/peers` — list every registered federation peer.
///
/// Requires an admin bearer token. Peers are returned sorted by
/// `(added_at ASC, peer_id ASC)` so the response is stable for
/// snapshot-style assertions.
pub(crate) async fn list_peers(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<PeerListResponse>, (StatusCode, String)> {
    require_admin(&state.cap_engine, &headers)?;

    let mut peers = state
        .store
        .peer_list_impl()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Stable order: (added_at ASC, peer_id ASC). Tie-break on peer_id
    // so equal timestamps don't surface non-deterministically.
    peers.sort_by(|a, b| a.added_at.cmp(&b.added_at).then(a.peer_id.cmp(&b.peer_id)));

    let items: Vec<PeerListItem> = peers.into_iter().map(PeerListItem::from).collect();
    Ok(Json(PeerListResponse { peers: items }))
}

/// `DELETE /v1/peers/:peer_id` — remove a federation peer and its
/// replication cursors.
///
/// Returns:
/// - `204 No Content` on a successful delete
/// - `404 Not Found` if no peer with that id exists
/// - `401`/`403` per [`require_admin`]
pub(crate) async fn remove_peer(
    State(state): State<AppState>,
    Path(peer_id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, (StatusCode, String)> {
    require_admin(&state.cap_engine, &headers)?;

    // `Store::peer_remove` is idempotent (Ok whether or not the row
    // existed), so we must look the peer up first to honor the 404
    // contract. Cost is fine: the peer table is tiny by design.
    let exists = state
        .store
        .peer_list_impl()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .iter()
        .any(|p| p.peer_id == peer_id);

    if !exists {
        return Err((StatusCode::NOT_FOUND, format!("no peer: {peer_id}")));
    }

    state
        .store
        .peer_remove_impl(&peer_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(StatusCode::NO_CONTENT)
}
