//! Daemon-side wire protocol server.
//!
//! The over-the-wire types (`Request`, `Response`, `BroadcastEvent`),
//! the length-prefix codec, and the consumer-facing `ProtocolClient`
//! live in the lean `ctxd-wire` crate so SDK clients can depend on the
//! protocol without inheriting the daemon's Store/Cap/MCP/HTTP stack.
//! This module owns only the server-side dispatch: TCP listener, per-
//! connection task, and the handlers that bind to `EventStore`,
//! `CapEngine`, and federation.
//!
//! Wire types are re-exported below so existing code (federation, main,
//! integration tests) can keep using `ctxd_cli::protocol::Request` etc.
//! without churn.

use crate::federation::PeerManager;
use crate::rate_limit::RateLimiter;
use ctxd_cap::{CapEngine, Operation};
use ctxd_core::event::Event;
use ctxd_core::subject::Subject;
use ctxd_store::EventStore;
use ctxd_wire::frame::{read_frame, write_frame};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, Mutex};

// Wire-level types live in ctxd-wire. Re-export them from this module
// so all existing imports (e.g. `use ctxd_cli::protocol::Request`)
// continue to resolve. The wire crate is the source of truth.
pub use ctxd_wire::{BroadcastEvent, ProtocolClient, Request, Response, SubscriptionStream};

/// The wire protocol TCP server.
pub struct ProtocolServer {
    store: EventStore,
    cap_engine: Arc<CapEngine>,
    addr: SocketAddr,
    event_tx: broadcast::Sender<BroadcastEvent>,
    /// Rate limiter for per-token throttling. Used by handle_connection
    /// when tokens carry a rate_limit_ops_per_sec fact.
    #[allow(dead_code)]
    rate_limiter: Arc<Mutex<RateLimiter>>,
    /// Optional federation manager. When set, federation Request
    /// variants are dispatched here instead of returning the legacy
    /// "not yet wired" error. When None, federation requests still
    /// produce a structured error so callers can detect the daemon
    /// has federation off.
    federation: Option<Arc<PeerManager>>,
}

impl ProtocolServer {
    /// Create a new protocol server.
    pub fn new(store: EventStore, cap_engine: Arc<CapEngine>, addr: SocketAddr) -> Self {
        let (event_tx, _) = broadcast::channel(1024);
        Self {
            store,
            cap_engine,
            addr,
            event_tx,
            rate_limiter: Arc::new(Mutex::new(RateLimiter::new())),
            federation: None,
        }
    }

    /// Attach a federation manager to this server. Federation requests
    /// (`PeerHello`, `PeerReplicate`, `PeerCursorRequest`, …) will be
    /// dispatched to it.
    pub fn with_federation(mut self, fed: Arc<PeerManager>) -> Self {
        self.federation = Some(fed);
        self
    }

    /// Get a broadcast sender for publishing events from other parts of the system.
    pub fn event_sender(&self) -> broadcast::Sender<BroadcastEvent> {
        self.event_tx.clone()
    }

    /// Run the protocol server, accepting TCP connections. Binds the
    /// server's stored address. For tests that need ephemeral-port
    /// orchestration, see [`Self::run_with_listener`].
    pub async fn run(self) -> anyhow::Result<()> {
        let listener = TcpListener::bind(self.addr).await?;
        self.run_with_listener(listener).await
    }

    /// Run the protocol server with a pre-bound listener. Used by
    /// federation integration tests that pick an ephemeral port via
    /// `TcpListener::bind("127.0.0.1:0")` and then read `local_addr()`
    /// without releasing the socket.
    pub async fn run_with_listener(self, listener: TcpListener) -> anyhow::Result<()> {
        tracing::info!(
            "Wire protocol server listening on {}",
            listener.local_addr()?
        );

        let store = Arc::new(self.store);
        let cap_engine = self.cap_engine;
        let event_tx = self.event_tx;
        let federation = self.federation;

        loop {
            let (stream, peer) = listener.accept().await?;
            tracing::debug!("Wire protocol connection from {peer}");

            let store = Arc::clone(&store);
            let cap_engine = Arc::clone(&cap_engine);
            let event_tx = event_tx.clone();
            let federation = federation.clone();

            tokio::spawn(async move {
                if let Err(e) =
                    handle_connection(stream, store, cap_engine, event_tx, federation).await
                {
                    tracing::warn!("Wire protocol connection error from {peer}: {e}");
                }
            });
        }
    }
}

/// Send a Response over the stream.
async fn send_response(stream: &mut TcpStream, response: &Response) -> anyhow::Result<()> {
    let data = rmp_serde::to_vec(response)?;
    write_frame(stream, &data).await?;
    Ok(())
}

/// Handle a single TCP connection.
pub(crate) async fn handle_connection(
    mut stream: TcpStream,
    store: Arc<EventStore>,
    cap_engine: Arc<CapEngine>,
    event_tx: broadcast::Sender<BroadcastEvent>,
    federation: Option<Arc<PeerManager>>,
) -> anyhow::Result<()> {
    loop {
        let frame = match read_frame(&mut stream).await? {
            Some(f) => f,
            None => return Ok(()), // client disconnected
        };

        let request: Request = rmp_serde::from_slice(&frame)?;

        match request {
            Request::Ping => {
                send_response(&mut stream, &Response::Pong).await?;
            }

            Request::Pub {
                subject,
                event_type,
                data,
            } => {
                let response =
                    match handle_pub(&store, &event_tx, &subject, &event_type, data).await {
                        Ok(resp) => resp,
                        Err(e) => Response::Error {
                            message: e.to_string(),
                        },
                    };
                send_response(&mut stream, &response).await?;
            }

            Request::Sub { subject_pattern } => {
                handle_sub(&mut stream, &event_tx, &subject_pattern).await?;
                // SUB keeps the connection open for streaming, then returns.
                return Ok(());
            }

            Request::Query {
                subject_pattern,
                view,
            } => {
                let response = match handle_query(&store, &subject_pattern, &view).await {
                    Ok(resp) => resp,
                    Err(e) => Response::Error {
                        message: e.to_string(),
                    },
                };
                send_response(&mut stream, &response).await?;
            }

            Request::Grant {
                subject,
                operations,
                expiry,
            } => {
                let response = match handle_grant(&cap_engine, &subject, &operations, &expiry) {
                    Ok(resp) => resp,
                    Err(e) => Response::Error {
                        message: e.to_string(),
                    },
                };
                send_response(&mut stream, &response).await?;
            }

            Request::Revoke { cap_id: _ } => {
                send_response(
                    &mut stream,
                    &Response::Error {
                        message: "REVOKE is not implemented, scheduled for v0.2".to_string(),
                    },
                )
                .await?;
            }

            // Federation verbs. Dispatch to `PeerManager` if attached,
            // otherwise return a structured error.
            Request::PeerHello {
                peer_id,
                public_key,
                offered_cap,
                subjects,
            } => {
                let response = match federation.as_ref() {
                    Some(fed) => match fed
                        .handle_peer_hello(&peer_id, &public_key, &offered_cap, &subjects)
                        .await
                    {
                        Ok(data) => Response::Ok { data },
                        Err(e) => Response::Error {
                            message: e.to_string(),
                        },
                    },
                    None => Response::Error {
                        message: "federation not enabled on this daemon".into(),
                    },
                };
                send_response(&mut stream, &response).await?;
            }

            Request::PeerReplicate {
                origin_peer_id,
                event,
            } => {
                let response = match federation.as_ref() {
                    Some(fed) => match fed.handle_peer_replicate(&origin_peer_id, event).await {
                        Ok(data) => Response::Ok { data },
                        Err(e) => Response::Error {
                            message: e.to_string(),
                        },
                    },
                    None => Response::Error {
                        message: "federation not enabled on this daemon".into(),
                    },
                };
                send_response(&mut stream, &response).await?;
            }

            Request::PeerCursorRequest {
                peer_id,
                subject_pattern,
            } => {
                let response = match federation.as_ref() {
                    Some(fed) => match fed
                        .handle_peer_cursor_request(&peer_id, &subject_pattern)
                        .await
                    {
                        Ok(data) => Response::Ok { data },
                        Err(e) => Response::Error {
                            message: e.to_string(),
                        },
                    },
                    None => Response::Error {
                        message: "federation not enabled on this daemon".into(),
                    },
                };
                send_response(&mut stream, &response).await?;
            }

            Request::PeerFetchEvents { event_ids } => {
                let response = match federation.as_ref() {
                    Some(fed) => match fed.handle_peer_fetch_events(&event_ids).await {
                        Ok(data) => Response::Ok { data },
                        Err(e) => Response::Error {
                            message: e.to_string(),
                        },
                    },
                    None => Response::Error {
                        message: "federation not enabled on this daemon".into(),
                    },
                };
                send_response(&mut stream, &response).await?;
            }

            // PeerWelcome, PeerAck, PeerCursor are response-shaped — a
            // peer should not be sending us these as the head of a
            // request. We respond with a clear error rather than trying
            // to interpret them.
            Request::PeerWelcome { .. } | Request::PeerAck { .. } | Request::PeerCursor { .. } => {
                send_response(
                    &mut stream,
                    &Response::Error {
                        message:
                            "PeerWelcome/PeerAck/PeerCursor are response variants, not requests"
                                .into(),
                    },
                )
                .await?;
            }
        }
    }
}

/// Handle a PUB request: append event and broadcast.
async fn handle_pub(
    store: &EventStore,
    event_tx: &broadcast::Sender<BroadcastEvent>,
    subject: &str,
    event_type: &str,
    data: serde_json::Value,
) -> anyhow::Result<Response> {
    let subject_parsed = Subject::new(subject)?;
    let event = Event::new(
        "ctxd://wire".to_string(),
        subject_parsed,
        event_type.to_string(),
        data,
    );

    let stored = store.append(event).await?;
    let event_json = serde_json::to_value(&stored)?;

    // Broadcast to subscribers (ignore errors if no receivers). Origin
    // is empty here — `handle_pub` is the local-PUB path; federation
    // receivers tag inbound events with their actual origin via the
    // `PeerReplicate` envelope before re-broadcasting.
    let _ = event_tx.send(BroadcastEvent {
        subject: subject.to_string(),
        event: event_json.clone(),
        origin_peer_id: String::new(),
    });

    Ok(Response::Ok { data: event_json })
}

/// Handle a SUB request: stream matching events to the client.
async fn handle_sub(
    stream: &mut TcpStream,
    event_tx: &broadcast::Sender<BroadcastEvent>,
    subject_pattern: &str,
) -> anyhow::Result<()> {
    let mut rx = event_tx.subscribe();
    let pattern = subject_pattern.to_string();

    loop {
        match rx.recv().await {
            Ok(broadcast_event) => {
                if subject_matches_pattern(&broadcast_event.subject, &pattern) {
                    let response = Response::Event {
                        event: broadcast_event.event,
                    };
                    if send_response(stream, &response).await.is_err() {
                        // Client disconnected.
                        return Ok(());
                    }
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!("subscriber lagged, missed {n} events");
            }
            Err(broadcast::error::RecvError::Closed) => {
                send_response(stream, &Response::EndOfStream).await?;
                return Ok(());
            }
        }
    }
}

/// Handle a QUERY request: read events or search.
async fn handle_query(
    store: &EventStore,
    subject_pattern: &str,
    view: &str,
) -> anyhow::Result<Response> {
    match view {
        "log" => {
            let subject = Subject::new(subject_pattern)?;
            let events = store.read(&subject, true).await?;
            let data: Vec<serde_json::Value> = events
                .iter()
                .filter_map(|e| serde_json::to_value(e).ok())
                .collect();
            Ok(Response::Ok {
                data: serde_json::Value::Array(data),
            })
        }
        "kv" => {
            let value = store.kv_get(subject_pattern).await?;
            Ok(Response::Ok {
                data: value.unwrap_or(serde_json::Value::Null),
            })
        }
        "fts" => {
            let events = store.search(subject_pattern, None).await?;
            let data: Vec<serde_json::Value> = events
                .iter()
                .filter_map(|e| serde_json::to_value(e).ok())
                .collect();
            Ok(Response::Ok {
                data: serde_json::Value::Array(data),
            })
        }
        other => Ok(Response::Error {
            message: format!("unknown view: {other}. Supported: log, kv, fts"),
        }),
    }
}

/// Handle a GRANT request: mint a capability token.
fn handle_grant(
    cap_engine: &CapEngine,
    subject: &str,
    operations: &[String],
    expiry: &Option<String>,
) -> anyhow::Result<Response> {
    let ops: Result<Vec<Operation>, _> = operations
        .iter()
        .map(|op| match op.as_str() {
            "read" => Ok(Operation::Read),
            "write" => Ok(Operation::Write),
            "subjects" => Ok(Operation::Subjects),
            "search" => Ok(Operation::Search),
            "admin" => Ok(Operation::Admin),
            other => anyhow::bail!("unknown operation: {other}"),
        })
        .collect();
    let ops = ops?;

    let expires_at = match expiry {
        Some(exp) => {
            let dt = chrono::DateTime::parse_from_rfc3339(exp)?;
            Some(dt.with_timezone(&chrono::Utc))
        }
        None => None,
    };

    let token = cap_engine.mint(subject, &ops, expires_at, None, None)?;
    let encoded = CapEngine::token_to_base64(&token);

    Ok(Response::Ok {
        data: serde_json::json!({ "token": encoded }),
    })
}

/// Check if a subject matches a pattern (delegates to ctxd_core).
fn subject_matches_pattern(subject: &str, pattern: &str) -> bool {
    ctxd_core::subject::Subject::matches_cap_pattern(subject, pattern)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subject_pattern_matching() {
        assert!(subject_matches_pattern("/test/hello", "/**"));
        assert!(subject_matches_pattern("/test/hello", "/test/**"));
        assert!(subject_matches_pattern("/test/hello", "/test/*"));
        assert!(subject_matches_pattern("/test/hello", "/test/hello"));
        assert!(!subject_matches_pattern("/test/hello", "/other/**"));
        assert!(!subject_matches_pattern("/test/a/b", "/test/*"));
        assert!(subject_matches_pattern("/test/a/b", "/test/**"));
    }

    #[tokio::test]
    async fn wire_protocol_pub_then_query_log() {
        let store = EventStore::open_memory().await.expect("open store");
        let cap_engine = Arc::new(CapEngine::new());
        let addr: SocketAddr = "127.0.0.1:0".parse().expect("parse addr");
        let listener = tokio::net::TcpListener::bind(addr).await.expect("bind");
        let bound_addr = listener.local_addr().expect("local_addr");

        let server_store = store.clone();
        let server_cap = cap_engine.clone();
        let server_handle = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let store = Arc::new(server_store);
            let (event_tx, _) = broadcast::channel(1024);
            handle_connection(stream, store, server_cap, event_tx, None)
                .await
                .expect("handle_connection");
        });

        let mut client = ProtocolClient::connect(&bound_addr.to_string())
            .await
            .expect("connect");

        // PUB an event
        let pub_resp = client
            .publish("/test/wire", "demo", serde_json::json!({"msg": "hello"}))
            .await
            .expect("publish");
        assert!(matches!(pub_resp, Response::Ok { .. }));

        // QUERY it back via "log" view
        let query_resp = client.query("/test/wire", "log").await.expect("query");
        match query_resp {
            Response::Ok { data } => {
                let arr = data.as_array().expect("array");
                assert_eq!(arr.len(), 1);
                assert_eq!(arr[0]["data"]["msg"], "hello");
            }
            other => panic!("expected Ok, got {other:?}"),
        }

        drop(client);
        let _ = server_handle.await;
    }

    #[tokio::test]
    async fn wire_protocol_sub_receives_pub() {
        // Test the SUB/PUB broadcast mechanism using a shared ProtocolServer
        // that handles both connections on the same broadcast channel.
        let store = EventStore::open_memory().await.expect("open store");
        let cap_engine = Arc::new(CapEngine::new());
        let (event_tx, _) = broadcast::channel::<BroadcastEvent>(1024);

        // Use a single listener that accepts both connections sequentially.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let bound_addr = listener.local_addr().expect("local_addr");

        let store_clone = store.clone();
        let cap_clone = cap_engine.clone();
        let event_tx_clone = event_tx.clone();
        let server_handle = tokio::spawn(async move {
            let store = Arc::new(store_clone);
            // Accept two connections
            for _ in 0..2 {
                let (stream, _) = listener.accept().await.expect("accept");
                let s = Arc::clone(&store);
                let c = Arc::clone(&cap_clone);
                let tx = event_tx_clone.clone();
                tokio::spawn(async move {
                    let _ = handle_connection(stream, s, c, tx, None).await;
                });
            }
        });

        // Connect subscriber first
        let sub_client = ProtocolClient::connect(&bound_addr.to_string())
            .await
            .expect("connect sub");
        let mut sub_stream = sub_client.subscribe("/test/**").await.expect("subscribe");

        // Give the subscription a moment to register with the broadcast channel
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Connect publisher and PUB an event
        let mut pub_client = ProtocolClient::connect(&bound_addr.to_string())
            .await
            .expect("connect pub");
        let _resp = pub_client
            .publish("/test/sub-test", "demo", serde_json::json!({"sub": "test"}))
            .await
            .expect("publish");

        // Subscriber should receive the event within 5 seconds
        let received =
            tokio::time::timeout(std::time::Duration::from_secs(5), sub_stream.next_event())
                .await
                .expect("timed out waiting for subscription event")
                .expect("next_event");

        match received {
            Some(Response::Event { event }) => {
                assert_eq!(event["data"]["sub"], "test");
            }
            other => panic!("expected Event, got {other:?}"),
        }

        drop(pub_client);
        drop(sub_stream);
        server_handle.abort();
    }

    #[tokio::test]
    async fn wire_protocol_grant_returns_valid_base64_biscuit() {
        let store = EventStore::open_memory().await.expect("open store");
        let cap_engine = Arc::new(CapEngine::new());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let bound_addr = listener.local_addr().expect("local_addr");

        let server_store = store.clone();
        let server_cap = cap_engine.clone();
        let server_handle = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let store = Arc::new(server_store);
            let (event_tx, _) = broadcast::channel(1024);
            handle_connection(stream, store, server_cap, event_tx, None)
                .await
                .expect("handle_connection");
        });

        let mut client = ProtocolClient::connect(&bound_addr.to_string())
            .await
            .expect("connect");

        let resp = client
            .grant("/**", &["read", "write"], None)
            .await
            .expect("grant");
        match resp {
            Response::Ok { data } => {
                let token_b64 = data["token"].as_str().expect("token");
                // Verify it is valid base64
                let token_bytes = CapEngine::token_from_base64(token_b64).expect("base64");
                // Verify the token can be verified by the cap engine
                cap_engine
                    .verify(&token_bytes, "/test", Operation::Read, None)
                    .expect("verify");
            }
            other => panic!("expected Ok, got {other:?}"),
        }

        drop(client);
        let _ = server_handle.await;
    }
}
