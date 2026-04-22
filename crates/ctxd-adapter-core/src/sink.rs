//! Direct sink implementation that writes events to an in-process EventStore.

use crate::{AdapterError, EventSink};
use ctxd_core::event::Event;
use ctxd_core::subject::Subject;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Type alias for the synchronous append callback used by [`DirectSink`].
type AppendFn = Box<dyn FnMut(Event) -> Result<String, AdapterError> + Send>;

/// A sink that writes events directly to a store callback.
///
/// This is used for in-process adapters where the adapter runs in the same
/// process as the ctxd daemon and can write events directly without going
/// through the network.
pub struct DirectSink {
    source: String,
    append_fn: Arc<Mutex<AppendFn>>,
}

/// A type-erased async append function for the direct sink.
///
/// We use a trait object to avoid coupling this crate to ctxd-store directly.
#[async_trait::async_trait]
pub trait AppendEvent: Send + Sync {
    /// Append an event and return its ID.
    async fn append(&self, event: Event) -> Result<String, AdapterError>;
}

/// A direct sink that wraps an [`AppendEvent`] implementation.
pub struct AsyncDirectSink {
    source: String,
    appender: Arc<dyn AppendEvent>,
}

impl AsyncDirectSink {
    /// Create a new async direct sink.
    ///
    /// # Arguments
    /// * `source` - The CloudEvents source URI (e.g., `ctxd://localhost`)
    /// * `appender` - The event appender implementation
    pub fn new(source: String, appender: Arc<dyn AppendEvent>) -> Self {
        Self { source, appender }
    }
}

#[async_trait::async_trait]
impl EventSink for AsyncDirectSink {
    async fn publish(
        &self,
        subject: &str,
        event_type: &str,
        data: Value,
    ) -> Result<String, AdapterError> {
        let subject = Subject::new(subject)
            .map_err(|e| AdapterError::Internal(format!("invalid subject: {e}")))?;
        let event = Event::new(self.source.clone(), subject, event_type.to_string(), data);
        self.appender.append(event).await
    }
}

impl DirectSink {
    /// Create a new direct sink with a synchronous append callback.
    ///
    /// # Arguments
    /// * `source` - The CloudEvents source URI (e.g., `ctxd://localhost`)
    /// * `append_fn` - A callback that appends the event and returns its ID
    pub fn new(
        source: String,
        append_fn: Box<dyn FnMut(Event) -> Result<String, AdapterError> + Send>,
    ) -> Self {
        Self {
            source,
            append_fn: Arc::new(Mutex::new(append_fn)),
        }
    }
}

#[async_trait::async_trait]
impl EventSink for DirectSink {
    async fn publish(
        &self,
        subject: &str,
        event_type: &str,
        data: Value,
    ) -> Result<String, AdapterError> {
        let subject = Subject::new(subject)
            .map_err(|e| AdapterError::Internal(format!("invalid subject: {e}")))?;
        let event = Event::new(self.source.clone(), subject, event_type.to_string(), data);
        let mut f = self.append_fn.lock().await;
        (f)(event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// Test AppendEvent implementation that collects events.
    struct CollectingAppender {
        events: Arc<Mutex<Vec<Event>>>,
    }

    #[async_trait::async_trait]
    impl AppendEvent for CollectingAppender {
        async fn append(&self, event: Event) -> Result<String, AdapterError> {
            let id = event.id.to_string();
            let mut guard = self.events.lock().await;
            guard.push(event);
            Ok(id)
        }
    }

    #[tokio::test]
    async fn async_direct_sink_publishes_event() {
        let collected: Arc<Mutex<Vec<Event>>> = Arc::new(Mutex::new(Vec::new()));
        let appender = Arc::new(CollectingAppender {
            events: collected.clone(),
        });

        let sink = AsyncDirectSink::new("ctxd://test".to_string(), appender);

        let id = sink
            .publish("/test/hello", "demo", serde_json::json!({"msg": "world"}))
            .await
            .unwrap();

        assert!(!id.is_empty());
        let events = collected.lock().await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "demo");
        assert_eq!(events[0].subject.as_str(), "/test/hello");
    }

    #[tokio::test]
    async fn direct_sink_publishes_event() {
        let collected: Arc<std::sync::Mutex<Vec<Event>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let collected_clone = collected.clone();

        let sink = DirectSink::new(
            "ctxd://test".to_string(),
            Box::new(move |event: Event| {
                let id = event.id.to_string();
                let mut guard = collected_clone.lock().unwrap();
                guard.push(event);
                Ok(id)
            }),
        );

        let id = sink
            .publish("/test/hello", "demo", serde_json::json!({"msg": "world"}))
            .await
            .unwrap();

        assert!(!id.is_empty());
        let events = collected.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "demo");
        assert_eq!(events[0].subject.as_str(), "/test/hello");
    }
}
