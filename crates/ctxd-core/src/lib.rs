//! Core types for ctxd: events, subjects, and hash chains.
//!
//! This crate has zero dependencies on storage, networking, or authorization.
//! Everything here is pure data types and algorithms.

pub mod event;
pub mod hash;
pub mod subject;

pub use event::Event;
pub use hash::PredecessorHash;
pub use subject::Subject;
