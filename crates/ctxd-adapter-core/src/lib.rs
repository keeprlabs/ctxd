//! Shared adapter infrastructure for ctxd ingestion adapters.
//!
//! This crate provides the [`Adapter`] and [`EventSink`] traits that all
//! ingestion adapters implement, plus a [`DirectSink`] for in-process use
//! that writes events directly to an [`ctxd_core::event::Event`]-based store.

mod sink;

pub use sink::{AppendEvent, AsyncDirectSink, DirectSink};

use serde_json::Value;

/// Errors that can occur in adapter operations.
#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    /// An I/O error occurred.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// A serialization or deserialization error occurred.
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// The adapter encountered an unexpected internal error.
    #[error("adapter error: {0}")]
    Internal(String),

    /// The adapter was cancelled or shut down.
    #[error("adapter cancelled")]
    Cancelled,
}

/// A sink that adapters use to publish events into ctxd.
///
/// Implementations may write directly to an in-process event store
/// ([`DirectSink`]) or send events over the network to a remote daemon.
#[async_trait::async_trait]
pub trait EventSink: Send + Sync {
    /// Publish an event.
    ///
    /// Returns the event ID on success.
    ///
    /// # Arguments
    /// * `subject` - The subject path, e.g. `/work/local/files/readme.md`
    /// * `event_type` - The event type, e.g. `file.created`
    /// * `data` - The event payload as a JSON value
    async fn publish(
        &self,
        subject: &str,
        event_type: &str,
        data: Value,
    ) -> Result<String, AdapterError>;
}

/// The core trait that all ingestion adapters implement.
///
/// An adapter watches an external data source (filesystem, email, API, etc.)
/// and publishes events into ctxd via an [`EventSink`].
#[async_trait::async_trait]
pub trait Adapter: Send + Sync {
    /// Human-readable name of this adapter.
    fn name(&self) -> &str;

    /// Subject prefix this adapter writes under (e.g., "/work/local").
    fn subject_prefix(&self) -> &str;

    /// Run the adapter, publishing events via the provided sink.
    ///
    /// This method should run until cancelled or until an unrecoverable
    /// error occurs. Implementations should handle transient errors
    /// gracefully (e.g., retry, log and continue).
    async fn run(&self, sink: Box<dyn EventSink>) -> Result<(), AdapterError>;
}
