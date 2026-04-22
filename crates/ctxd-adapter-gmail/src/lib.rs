//! Gmail adapter for ctxd.
//!
//! When fully implemented, this adapter will:
//! - Connect to the Gmail API using OAuth2 credentials
//! - Poll for new emails (or use push notifications via Google Pub/Sub)
//! - Publish events under `/work/email/gmail/{label}/{message_id}`
//! - Event types: `email.received`, `email.sent`, `email.archived`
//! - Event data includes: sender, recipients, subject, body (text), date, labels, thread ID
//! - Support incremental sync using Gmail history IDs
//! - Respect rate limits and handle token refresh

use ctxd_adapter_core::{Adapter, AdapterError, EventSink};

/// Gmail adapter that ingests emails via the Gmail API.
pub struct GmailAdapter {
    /// The Gmail user ID (typically "me" for the authenticated user).
    user_id: String,
}

impl GmailAdapter {
    /// Create a new Gmail adapter.
    ///
    /// # Arguments
    /// * `user_id` - The Gmail user ID (typically "me")
    pub fn new(user_id: String) -> Self {
        Self { user_id }
    }
}

#[async_trait::async_trait]
impl Adapter for GmailAdapter {
    fn name(&self) -> &str {
        "gmail"
    }

    fn subject_prefix(&self) -> &str {
        "/work/email/gmail"
    }

    async fn run(&self, _sink: Box<dyn EventSink>) -> Result<(), AdapterError> {
        todo!("Gmail adapter not yet implemented — requires OAuth2 setup and Gmail API integration for user {}", self.user_id)
    }
}
