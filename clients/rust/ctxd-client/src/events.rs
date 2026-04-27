//! Event re-exports.
//!
//! The SDK does not own its own [`Event`] type. We re-export
//! [`ctxd_core::event::Event`] so consumers serialize / deserialize the
//! exact same struct the daemon uses on its wire format. Re-rolling a
//! parallel SDK type would split the schema and is exactly the bug
//! every published "OpenAPI generator" SDK ships with.

pub use ctxd_core::event::Event;
pub use ctxd_core::subject::Subject;

/// Convenience type alias: every event in ctxd is identified by a
/// UUIDv7. Re-exported so SDK callers don't need to depend on `uuid`
/// directly to talk about event IDs.
pub type EventId = uuid::Uuid;
