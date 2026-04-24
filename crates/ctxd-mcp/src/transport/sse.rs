//! Legacy MCP HTTP/SSE transport.
//!
//! ## Wire shape
//!
//! This is the pre-streamable-HTTP transport, still in wide use by
//! agent frameworks that haven't moved to the 2025-03-26 unified
//! endpoint. Two HTTP routes:
//!
//! * `GET /sse` — open an `text/event-stream` response. The first
//!   event is `endpoint` whose `data` is a relative URL like
//!   `/messages?sessionId=…` that the client must POST subsequent
//!   JSON-RPC requests to. All server→client messages are emitted as
//!   `message` events on this stream.
//! * `POST /messages?sessionId=…` — JSON-RPC request body. The server
//!   accepts the message, processes it asynchronously, and replies via
//!   the SSE stream identified by `sessionId`. The HTTP response is
//!   `202 Accepted` with an empty body.
//!
//! ## Why we hand-roll this
//!
//! rmcp 1.5 ships a server for the *new* streamable-HTTP transport but
//! does not ship a server for the legacy SSE transport (the older
//! `mcp-server-side-sse` crate was folded back into the project but
//! the server-side SSE module isn't on crates.io as of rmcp 1.5.0).
//! We implement our own thin layer here that reuses rmcp's
//! [`rmcp::transport::Transport`] trait to plug into
//! [`rmcp::service::serve_directly`], so the actual MCP framing is
//! still rmcp-managed.
//!
//! ## Auth
//!
//! Uses the same [`super::auth_middleware`] as streamable-HTTP. Headers
//! presented on `POST /messages` are honored (Bearer beats the per-call
//! `token` arg). Auth on `GET /sse` itself is allowed without a token —
//! opening a session never carries privileged data; only the POSTs that
//! flow through it can mutate state, and those go through the
//! middleware.

use crate::auth::AuthPolicy;
use crate::transport::auth_middleware::{auth_layer, AuthMiddlewareConfig};
use crate::CtxdMcpServer;
use axum::{
    body::Body,
    extract::{Query, State},
    http::{HeaderMap, Request, StatusCode},
    response::sse::{Event, KeepAlive, Sse},
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use futures::stream::{Stream, StreamExt};
use rmcp::model::{ClientJsonRpcMessage, ServerJsonRpcMessage};
use rmcp::service::{serve_directly, RxJsonRpcMessage, TxJsonRpcMessage};
use rmcp::transport::Transport;
use rmcp::RoleServer;
use serde::Deserialize;
use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tokio_util::sync::CancellationToken;

/// In-flight SSE sessions, keyed by session_id (UUID v4 hex).
///
/// Each session owns a sender into its associated rmcp transport — POSTs
/// to `/messages?sessionId=…` forward into that sender. A mutex-protected
/// map is fine for the workloads ctxd targets (low-double-digit
/// concurrent SSE clients per daemon).
type Sessions = Arc<RwLock<HashMap<String, mpsc::Sender<RxJsonRpcMessage<RoleServer>>>>>;

#[derive(Clone)]
struct SseState {
    server: CtxdMcpServer,
    sessions: Sessions,
    cancel: CancellationToken,
}

/// Run the legacy SSE MCP transport on `addr`.
///
/// `policy` controls whether `tools/call` requests on the messages
/// endpoint require an `Authorization` header or `token` argument.
/// `cancel` triggers graceful shutdown.
pub async fn run_sse(
    server: CtxdMcpServer,
    addr: SocketAddr,
    policy: AuthPolicy,
    cancel: CancellationToken,
) -> Result<(), super::TransportError> {
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| super::TransportError::Startup(format!("bind {addr}: {e}")))?;
    let bound = listener
        .local_addr()
        .map_err(|e| super::TransportError::Startup(format!("local_addr: {e}")))?;
    tracing::info!(addr = %bound, "SSE MCP listening");

    let state = SseState {
        server,
        sessions: Arc::new(RwLock::new(HashMap::new())),
        cancel: cancel.clone(),
    };

    let auth_cfg = AuthMiddlewareConfig::new(policy);

    let app = Router::new()
        .route("/sse", get(sse_handler))
        .route("/messages", post(messages_handler))
        .with_state(state)
        .layer(axum::middleware::from_fn_with_state(
            auth_cfg,
            auth_layer,
        ));

    let make_service = app.into_make_service_with_connect_info::<SocketAddr>();
    let serve_fut = axum::serve(listener, make_service)
        .with_graceful_shutdown(async move { cancel.cancelled_owned().await });

    serve_fut
        .await
        .map_err(|e| super::TransportError::Runtime(format!("axum serve: {e}")))?;
    Ok(())
}

/// `GET /sse`: open a session and emit an `endpoint` event followed by
/// any server→client messages produced by tool calls posted to
/// `/messages?sessionId=…`.
async fn sse_handler(
    State(state): State<SseState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // Tracing: client connection. We don't have remote_addr at hand here
    // because axum middleware already consumed the ConnectInfo extraction
    // earlier (its `from_fn_with_state` does not propagate by default for
    // sub-routes). Logging "method" + "path" is enough for ops.
    let _ = headers; // header values are intentionally not logged
    let session_id = uuid::Uuid::new_v4().simple().to_string();

    // Channels: rx side is fed from POST /messages; tx side feeds the SSE
    // response stream. rmcp's `serve_directly` runs the handler against
    // a transport built from these.
    let (in_tx, in_rx) = mpsc::channel::<RxJsonRpcMessage<RoleServer>>(64);
    let (out_tx, out_rx) =
        mpsc::channel::<TxJsonRpcMessage<RoleServer>>(64);

    state
        .sessions
        .write()
        .await
        .insert(session_id.clone(), in_tx);

    let transport = SseTransport {
        rx: in_rx,
        tx: out_tx,
    };
    let service = state.server.clone();
    let cancel = state.cancel.child_token();
    let session_for_cleanup = session_id.clone();
    let sessions_for_cleanup = state.sessions.clone();

    tokio::spawn(async move {
        // `serve_directly` skips MCP initialise — but the client will
        // explicitly send `initialize` over our transport, which the
        // service handles. (We model SSE as if it has already been
        // initialised in the sense of "no separate handshake on the
        // transport itself".)
        let running = serve_directly(service, transport, None);
        tokio::select! {
            _ = running.waiting() => {}
            _ = cancel.cancelled() => {}
        }
        sessions_for_cleanup.write().await.remove(&session_for_cleanup);
        tracing::debug!(session_id = %session_for_cleanup, "SSE session ended");
    });

    let endpoint_path = format!("/messages?sessionId={session_id}");
    tracing::info!(session_id = %session_id, "SSE session opened");

    let endpoint_stream = futures::stream::once(async move {
        Ok::<_, Infallible>(Event::default().event("endpoint").data(endpoint_path))
    });
    let outbound_stream = OutboundStream { rx: out_rx };

    let stream = endpoint_stream.chain(outbound_stream);
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// SSE outbound stream: pull rmcp's outgoing messages from the channel
/// and serialise each as a `message` event.
struct OutboundStream {
    rx: mpsc::Receiver<ServerJsonRpcMessage>,
}

impl Stream for OutboundStream {
    type Item = Result<Event, Infallible>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        match self.rx.poll_recv(cx) {
            std::task::Poll::Ready(Some(msg)) => {
                let json = match serde_json::to_string(&msg) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!(error = %e, "SSE serialize failure");
                        return std::task::Poll::Ready(Some(Ok(Event::default()
                            .event("error")
                            .data("serialize_failed"))));
                    }
                };
                std::task::Poll::Ready(Some(Ok(Event::default().event("message").data(json))))
            }
            std::task::Poll::Ready(None) => std::task::Poll::Ready(None),
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

/// `POST /messages?sessionId=…` query string.
#[derive(Debug, Deserialize)]
struct MessagesQuery {
    #[serde(rename = "sessionId")]
    session_id: String,
}

/// `POST /messages?sessionId=…`: parse a JSON-RPC request and dispatch
/// it into the session's transport. Returns 202 Accepted; the response
/// flows back over the SSE stream.
async fn messages_handler(
    State(state): State<SseState>,
    Query(q): Query<MessagesQuery>,
    req: Request<Body>,
) -> impl IntoResponse {
    let bytes = match axum::body::to_bytes(req.into_body(), super::DEFAULT_MAX_BODY_BYTES).await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(error = %e, "SSE messages: body too large");
            return (StatusCode::PAYLOAD_TOO_LARGE, "Payload too large").into_response();
        }
    };

    let message: ClientJsonRpcMessage = match serde_json::from_slice(&bytes) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(error = %e, "SSE messages: bad JSON-RPC body");
            return (StatusCode::BAD_REQUEST, "Invalid JSON-RPC body").into_response();
        }
    };

    let sessions = state.sessions.read().await;
    let Some(tx) = sessions.get(&q.session_id) else {
        return (StatusCode::NOT_FOUND, "Unknown sessionId").into_response();
    };
    let tx = tx.clone();
    drop(sessions);

    if tx.send(message).await.is_err() {
        return (StatusCode::GONE, "Session terminated").into_response();
    }
    StatusCode::ACCEPTED.into_response()
}

/// Adapter: an rmcp `Transport<RoleServer>` backed by a pair of
/// tokio::mpsc channels. The receiver side is fed by the
/// `POST /messages` handler; the sender side is drained by the SSE
/// response stream.
struct SseTransport {
    rx: mpsc::Receiver<RxJsonRpcMessage<RoleServer>>,
    tx: mpsc::Sender<TxJsonRpcMessage<RoleServer>>,
}

#[derive(Debug, thiserror::Error)]
#[error("SSE transport channel closed")]
struct SseTransportError;

impl Transport<RoleServer> for SseTransport {
    type Error = SseTransportError;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleServer>,
    ) -> impl std::future::Future<Output = Result<(), Self::Error>> + Send + 'static {
        let tx = self.tx.clone();
        async move { tx.send(item).await.map_err(|_| SseTransportError) }
    }

    async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleServer>> {
        self.rx.recv().await
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        // Nothing to do — dropping the channels triggers cleanup
        // upstream (the outbound stream sees `None` and ends).
        Ok(())
    }
}
