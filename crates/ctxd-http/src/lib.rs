//! Admin REST API for ctxd.
//!
//! Provides a minimal axum-based HTTP server for admin operations:
//! - `GET /health` — health check
//! - `POST /v1/grant` — mint a capability token
//! - `GET /v1/stats` — basic store statistics

pub mod router;

pub use router::build_router;
