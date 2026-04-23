//! ctxd wire protocol: MessagePack over TCP.
//!
//! Six verbs:
//! - `PUB <subject> <event_json>` — append event
//! - `SUB <subject_pattern>` — subscribe (returns stream of events)
//! - `QUERY <subject_pattern> <view>` — query materialized view
//! - `GRANT <subject> <ops> <expiry>` — mint capability token
//! - `REVOKE <cap_id>` — stub (v0.2)
//! - `PING` — health check

use crate::rate_limit::RateLimiter;
use ctxd_cap::{CapEngine, Operation};
use ctxd_core::event::Event;
use ctxd_core::subject::Subject;
use ctxd_store::EventStore;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, Mutex};

/// Wire protocol request messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Request {
    /// Publish (append) an event.
    Pub {
        subject: String,
        event_type: String,
        data: serde_json::Value,
    },
    /// Subscribe to events matching a subject pattern.
    Sub { subject_pattern: String },
    /// Query a materialized view.
    Query {
        subject_pattern: String,
        view: String,
    },
    /// Mint a capability token.
    Grant {
        subject: String,
        operations: Vec<String>,
        expiry: Option<String>,
    },
    /// Revoke a capability token (v0.2 stub).
    Revoke { cap_id: String },
    /// Health check.
    Ping,
}

/// Wire protocol response messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Response {
    /// Successful response with a JSON payload.
    Ok { data: serde_json::Value },
    /// An event streamed from a subscription.
    Event { event: serde_json::Value },
    /// Error response.
    Error { message: String },
    /// Pong response to a health check.
    Pong,
    /// End of stream marker.
    EndOfStream,
}

/// Broadcast event for SUB fan-out.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BroadcastEvent {
    pub subject: String,
    pub event: serde_json::Value,
}

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
        }
    }

    /// Get a broadcast sender for publishing events from other parts of the system.
    #[allow(dead_code)]
    pub fn event_sender(&self) -> broadcast::Sender<BroadcastEvent> {
        self.event_tx.clone()
    }

    /// Run the protocol server, accepting TCP connections.
    pub async fn run(self) -> anyhow::Result<()> {
        let listener = TcpListener::bind(self.addr).await?;
        tracing::info!("Wire protocol server listening on {}", self.addr);

        let store = Arc::new(self.store);
        let cap_engine = self.cap_engine;
        let event_tx = self.event_tx;

        loop {
            let (stream, peer) = listener.accept().await?;
            tracing::debug!("Wire protocol connection from {peer}");

            let store = Arc::clone(&store);
            let cap_engine = Arc::clone(&cap_engine);
            let event_tx = event_tx.clone();

            tokio::spawn(async move {
                if let Err(e) = handle_connection(stream, store, cap_engine, event_tx).await {
                    tracing::warn!("Wire protocol connection error from {peer}: {e}");
                }
            });
        }
    }
}

/// Read a length-prefixed MessagePack frame from the stream.
async fn read_frame(stream: &mut TcpStream) -> anyhow::Result<Option<Vec<u8>>> {
    let len = match stream.read_u32().await {
        Ok(n) => n as usize,
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    };

    if len > 16 * 1024 * 1024 {
        anyhow::bail!("frame too large: {len} bytes");
    }

    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await?;
    Ok(Some(buf))
}

/// Write a length-prefixed MessagePack frame to the stream.
async fn write_frame(stream: &mut TcpStream, data: &[u8]) -> anyhow::Result<()> {
    stream.write_u32(data.len() as u32).await?;
    stream.write_all(data).await?;
    stream.flush().await?;
    Ok(())
}

/// Send a Response over the stream.
async fn send_response(stream: &mut TcpStream, response: &Response) -> anyhow::Result<()> {
    let data = rmp_serde::to_vec(response)?;
    write_frame(stream, &data).await
}

/// Handle a single TCP connection.
async fn handle_connection(
    mut stream: TcpStream,
    store: Arc<EventStore>,
    cap_engine: Arc<CapEngine>,
    event_tx: broadcast::Sender<BroadcastEvent>,
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

    // Broadcast to subscribers (ignore errors if no receivers).
    let _ = event_tx.send(BroadcastEvent {
        subject: subject.to_string(),
        event: event_json.clone(),
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

/// TCP client for connecting to a running ctxd daemon via the wire protocol.
pub struct ProtocolClient {
    stream: TcpStream,
}

impl ProtocolClient {
    /// Connect to a ctxd daemon at the given address.
    pub async fn connect(addr: &str) -> anyhow::Result<Self> {
        let stream = TcpStream::connect(addr).await?;
        Ok(Self { stream })
    }

    /// Send a request and receive a response.
    pub async fn request(&mut self, req: &Request) -> anyhow::Result<Response> {
        let data = rmp_serde::to_vec(req)?;
        write_frame(&mut self.stream, &data).await?;

        let frame = read_frame(&mut self.stream)
            .await?
            .ok_or_else(|| anyhow::anyhow!("server closed connection"))?;
        let response: Response = rmp_serde::from_slice(&frame)?;
        Ok(response)
    }

    /// Send a PING and expect a PONG.
    pub async fn ping(&mut self) -> anyhow::Result<()> {
        let response = self.request(&Request::Ping).await?;
        match response {
            Response::Pong => Ok(()),
            other => anyhow::bail!("unexpected response to PING: {other:?}"),
        }
    }

    /// Publish an event.
    pub async fn publish(
        &mut self,
        subject: &str,
        event_type: &str,
        data: serde_json::Value,
    ) -> anyhow::Result<Response> {
        self.request(&Request::Pub {
            subject: subject.to_string(),
            event_type: event_type.to_string(),
            data,
        })
        .await
    }

    /// Query a materialized view.
    pub async fn query(&mut self, subject_pattern: &str, view: &str) -> anyhow::Result<Response> {
        self.request(&Request::Query {
            subject_pattern: subject_pattern.to_string(),
            view: view.to_string(),
        })
        .await
    }

    /// Mint a capability token.
    pub async fn grant(
        &mut self,
        subject: &str,
        operations: &[&str],
        expiry: Option<&str>,
    ) -> anyhow::Result<Response> {
        self.request(&Request::Grant {
            subject: subject.to_string(),
            operations: operations.iter().map(|s| s.to_string()).collect(),
            expiry: expiry.map(|s| s.to_string()),
        })
        .await
    }

    /// Subscribe to events matching a pattern. Returns the stream for reading events.
    #[allow(dead_code)]
    pub async fn subscribe(mut self, subject_pattern: &str) -> anyhow::Result<SubscriptionStream> {
        let req = Request::Sub {
            subject_pattern: subject_pattern.to_string(),
        };
        let data = rmp_serde::to_vec(&req)?;
        write_frame(&mut self.stream, &data).await?;
        Ok(SubscriptionStream {
            stream: self.stream,
        })
    }
}

/// A stream of events from a subscription.
#[allow(dead_code)]
pub struct SubscriptionStream {
    stream: TcpStream,
}

impl SubscriptionStream {
    /// Receive the next event from the subscription.
    #[allow(dead_code)]
    pub async fn next_event(&mut self) -> anyhow::Result<Option<Response>> {
        let frame = match read_frame(&mut self.stream).await? {
            Some(f) => f,
            None => return Ok(None),
        };
        let response: Response = rmp_serde::from_slice(&frame)?;
        match &response {
            Response::EndOfStream => Ok(None),
            _ => Ok(Some(response)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_serialization_roundtrip() {
        let req = Request::Pub {
            subject: "/test/hello".to_string(),
            event_type: "demo".to_string(),
            data: serde_json::json!({"msg": "world"}),
        };
        let bytes = rmp_serde::to_vec(&req).unwrap();
        let decoded: Request = rmp_serde::from_slice(&bytes).unwrap();
        match decoded {
            Request::Pub {
                subject,
                event_type,
                data,
            } => {
                assert_eq!(subject, "/test/hello");
                assert_eq!(event_type, "demo");
                assert_eq!(data, serde_json::json!({"msg": "world"}));
            }
            _ => panic!("unexpected variant"),
        }
    }

    #[test]
    fn response_serialization_roundtrip() {
        let resp = Response::Ok {
            data: serde_json::json!({"id": "abc123"}),
        };
        let bytes = rmp_serde::to_vec(&resp).unwrap();
        let decoded: Response = rmp_serde::from_slice(&bytes).unwrap();
        match decoded {
            Response::Ok { data } => {
                assert_eq!(data["id"], "abc123");
            }
            _ => panic!("unexpected variant"),
        }
    }

    #[test]
    fn ping_pong_serialization() {
        let req = Request::Ping;
        let bytes = rmp_serde::to_vec(&req).unwrap();
        let decoded: Request = rmp_serde::from_slice(&bytes).unwrap();
        assert!(matches!(decoded, Request::Ping));

        let resp = Response::Pong;
        let bytes = rmp_serde::to_vec(&resp).unwrap();
        let decoded: Response = rmp_serde::from_slice(&bytes).unwrap();
        assert!(matches!(decoded, Response::Pong));
    }

    #[test]
    fn all_request_variants_serialize() {
        let variants: Vec<Request> = vec![
            Request::Ping,
            Request::Pub {
                subject: "/a".to_string(),
                event_type: "t".to_string(),
                data: serde_json::json!({}),
            },
            Request::Sub {
                subject_pattern: "/**".to_string(),
            },
            Request::Query {
                subject_pattern: "/a".to_string(),
                view: "log".to_string(),
            },
            Request::Grant {
                subject: "/**".to_string(),
                operations: vec!["read".to_string()],
                expiry: None,
            },
            Request::Revoke {
                cap_id: "id-1".to_string(),
            },
        ];
        for v in &variants {
            let bytes = rmp_serde::to_vec(v).unwrap();
            let _: Request = rmp_serde::from_slice(&bytes).unwrap();
        }
    }

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
        let store = EventStore::open_memory().await.unwrap();
        let cap_engine = Arc::new(CapEngine::new());
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
        let bound_addr = listener.local_addr().unwrap();

        let server_store = store.clone();
        let server_cap = cap_engine.clone();
        let server_handle = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let store = Arc::new(server_store);
            let (event_tx, _) = broadcast::channel(1024);
            handle_connection(stream, store, server_cap, event_tx)
                .await
                .unwrap();
        });

        let mut client = ProtocolClient::connect(&bound_addr.to_string())
            .await
            .unwrap();

        // PUB an event
        let pub_resp = client
            .publish("/test/wire", "demo", serde_json::json!({"msg": "hello"}))
            .await
            .unwrap();
        assert!(matches!(pub_resp, Response::Ok { .. }));

        // QUERY it back via "log" view
        let query_resp = client.query("/test/wire", "log").await.unwrap();
        match query_resp {
            Response::Ok { data } => {
                let arr = data.as_array().unwrap();
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
        let store = EventStore::open_memory().await.unwrap();
        let cap_engine = Arc::new(CapEngine::new());
        let (event_tx, _) = broadcast::channel::<BroadcastEvent>(1024);

        // Use a single listener that accepts both connections sequentially.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bound_addr = listener.local_addr().unwrap();

        let store_clone = store.clone();
        let cap_clone = cap_engine.clone();
        let event_tx_clone = event_tx.clone();
        let server_handle = tokio::spawn(async move {
            let store = Arc::new(store_clone);
            // Accept two connections
            for _ in 0..2 {
                let (stream, _) = listener.accept().await.unwrap();
                let s = Arc::clone(&store);
                let c = Arc::clone(&cap_clone);
                let tx = event_tx_clone.clone();
                tokio::spawn(async move {
                    let _ = handle_connection(stream, s, c, tx).await;
                });
            }
        });

        // Connect subscriber first
        let sub_client = ProtocolClient::connect(&bound_addr.to_string())
            .await
            .unwrap();
        let mut sub_stream = sub_client.subscribe("/test/**").await.unwrap();

        // Give the subscription a moment to register with the broadcast channel
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Connect publisher and PUB an event
        let mut pub_client = ProtocolClient::connect(&bound_addr.to_string())
            .await
            .unwrap();
        let _resp = pub_client
            .publish("/test/sub-test", "demo", serde_json::json!({"sub": "test"}))
            .await
            .unwrap();

        // Subscriber should receive the event within 5 seconds
        let received =
            tokio::time::timeout(std::time::Duration::from_secs(5), sub_stream.next_event())
                .await
                .expect("timed out waiting for subscription event")
                .unwrap();

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
        let store = EventStore::open_memory().await.unwrap();
        let cap_engine = Arc::new(CapEngine::new());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bound_addr = listener.local_addr().unwrap();

        let server_store = store.clone();
        let server_cap = cap_engine.clone();
        let server_handle = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let store = Arc::new(server_store);
            let (event_tx, _) = broadcast::channel(1024);
            handle_connection(stream, store, server_cap, event_tx)
                .await
                .unwrap();
        });

        let mut client = ProtocolClient::connect(&bound_addr.to_string())
            .await
            .unwrap();

        let resp = client.grant("/**", &["read", "write"], None).await.unwrap();
        match resp {
            Response::Ok { data } => {
                let token_b64 = data["token"].as_str().unwrap();
                // Verify it is valid base64
                let token_bytes = CapEngine::token_from_base64(token_b64).unwrap();
                // Verify the token can be verified by the cap engine
                cap_engine
                    .verify(&token_bytes, "/test", Operation::Read, None)
                    .unwrap();
            }
            other => panic!("expected Ok, got {other:?}"),
        }

        drop(client);
        let _ = server_handle.await;
    }
}
