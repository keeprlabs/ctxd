//! MCP server that wraps ctxd store operations as tools.

use ctxd_cap::{CapEngine, Operation};
use ctxd_core::event::Event;
use ctxd_core::subject::Subject;
use ctxd_store::EventStore;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::{tool, tool_router};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Arguments for ctx_write tool.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct WriteParams {
    /// Subject path for the event.
    pub subject: String,
    /// Event type descriptor.
    pub event_type: String,
    /// Event data as JSON string.
    pub data: String,
    /// Optional capability token (base64-encoded).
    pub token: Option<String>,
}

/// Result of ctx_write.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct WriteResult {
    /// The event ID.
    pub id: String,
    /// The subject path.
    pub subject: String,
    /// The predecessor hash, if any.
    pub predecessorhash: Option<String>,
}

/// Arguments for ctx_read tool.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReadParams {
    /// Subject path to read from.
    pub subject: String,
    /// Whether to read recursively.
    #[serde(default)]
    pub recursive: bool,
    /// Optional capability token (base64-encoded).
    pub token: Option<String>,
}

/// Arguments for ctx_subjects tool.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SubjectsParams {
    /// Optional prefix to filter subjects.
    pub prefix: Option<String>,
    /// Whether to list recursively.
    #[serde(default)]
    pub recursive: bool,
    /// Optional capability token (base64-encoded).
    pub token: Option<String>,
}

/// The ctxd MCP server.
#[derive(Clone)]
pub struct CtxdMcpServer {
    store: EventStore,
    cap_engine: Arc<CapEngine>,
    source: String,
}

impl CtxdMcpServer {
    /// Create a new MCP server wrapping the given store and capability engine.
    pub fn new(store: EventStore, cap_engine: Arc<CapEngine>, source: String) -> Self {
        Self {
            store,
            cap_engine,
            source,
        }
    }

    /// Verify a capability token if provided.
    fn verify_token(
        &self,
        token: &Option<String>,
        subject: &str,
        operation: Operation,
    ) -> Result<(), String> {
        if let Some(tok) = token {
            let bytes = CapEngine::token_from_base64(tok).map_err(|e| e.to_string())?;
            self.cap_engine
                .verify(&bytes, subject, operation)
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

#[tool_router(server_handler)]
impl CtxdMcpServer {
    /// Write a context event to the store.
    #[tool(
        name = "ctx_write",
        description = "Append a context event. Takes subject (path), event_type, data (JSON string), and optional token (capability)."
    )]
    async fn ctx_write(&self, Parameters(params): Parameters<WriteParams>) -> Json<WriteResult> {
        if let Err(e) = self.verify_token(&params.token, &params.subject, Operation::Write) {
            return Json(WriteResult {
                id: String::new(),
                subject: format!("error: {e}"),
                predecessorhash: None,
            });
        }

        let subject = match Subject::new(&params.subject) {
            Ok(s) => s,
            Err(e) => {
                return Json(WriteResult {
                    id: String::new(),
                    subject: format!("error: invalid subject: {e}"),
                    predecessorhash: None,
                });
            }
        };

        let data: serde_json::Value = match serde_json::from_str(&params.data) {
            Ok(d) => d,
            Err(e) => {
                return Json(WriteResult {
                    id: String::new(),
                    subject: format!("error: invalid JSON data: {e}"),
                    predecessorhash: None,
                });
            }
        };

        let event = Event::new(self.source.clone(), subject, params.event_type, data);

        match self.store.append(event).await {
            Ok(stored) => Json(WriteResult {
                id: stored.id.to_string(),
                subject: stored.subject.as_str().to_string(),
                predecessorhash: stored.predecessorhash,
            }),
            Err(e) => Json(WriteResult {
                id: String::new(),
                subject: format!("error: write failed: {e}"),
                predecessorhash: None,
            }),
        }
    }

    /// Read context events for a subject.
    #[tool(
        name = "ctx_read",
        description = "Read context events for a subject path. Takes subject (path), recursive (bool), and optional token."
    )]
    async fn ctx_read(&self, Parameters(params): Parameters<ReadParams>) -> String {
        if let Err(e) = self.verify_token(&params.token, &params.subject, Operation::Read) {
            return format!("error: {e}");
        }

        let subject = match Subject::new(&params.subject) {
            Ok(s) => s,
            Err(e) => return format!("error: invalid subject: {e}"),
        };

        match self.store.read(&subject, params.recursive).await {
            Ok(events) => {
                let output: Vec<serde_json::Value> = events
                    .iter()
                    .map(|e| serde_json::to_value(e).unwrap_or_default())
                    .collect();
                serde_json::to_string_pretty(&output).unwrap_or_default()
            }
            Err(e) => format!("error: read failed: {e}"),
        }
    }

    /// List context subjects.
    #[tool(
        name = "ctx_subjects",
        description = "List context subjects. Takes optional prefix (path), recursive (bool), and optional token."
    )]
    async fn ctx_subjects(&self, Parameters(params): Parameters<SubjectsParams>) -> String {
        let prefix = match &params.prefix {
            Some(p) => {
                if let Err(e) = self.verify_token(&params.token, p, Operation::Subjects) {
                    return format!("error: {e}");
                }
                match Subject::new(p) {
                    Ok(s) => Some(s),
                    Err(e) => return format!("error: invalid prefix: {e}"),
                }
            }
            None => None,
        };

        match self.store.subjects(prefix.as_ref(), params.recursive).await {
            Ok(subjects) => serde_json::to_string_pretty(&subjects).unwrap_or_default(),
            Err(e) => format!("error: subjects failed: {e}"),
        }
    }
}
