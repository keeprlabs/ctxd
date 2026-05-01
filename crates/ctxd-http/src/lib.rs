//! Admin REST API for ctxd.
//!
//! Provides a minimal axum-based HTTP server for admin operations:
//! - `GET /health` — health check
//! - `POST /v1/grant` — mint a capability token
//! - `GET /v1/stats` — basic store statistics
//! - `GET /v1/peers` — list federation peers (admin)
//! - `DELETE /v1/peers/:peer_id` — remove a federation peer (admin)
//! - `GET /v1/approvals` — list pending HumanApproval requests
//! - `POST /v1/approvals/:id/decide` — allow/deny an approval (admin)

pub mod handlers;
pub mod middleware;
pub mod responses;
pub mod router;

pub use responses::{PeerListItem, PeerListResponse};
pub use router::{build_router, AppState};
