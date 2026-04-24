//! ctxd — context substrate daemon for AI agents.

mod protocol;
mod query;
mod rate_limit;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use ctxd_cap::{CapEngine, Operation};
use ctxd_core::event::Event;
use ctxd_core::signing::EventSigner;
use ctxd_core::subject::Subject;
use ctxd_http::build_router;
use ctxd_mcp::CtxdMcpServer;
use ctxd_store::EventStore;
use opentelemetry::trace::TracerProvider;
use protocol::{ProtocolClient, ProtocolServer};
use rmcp::ServiceExt;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

/// ctxd — context substrate for AI agents
#[derive(Parser)]
#[command(name = "ctxd", version, about)]
struct Cli {
    /// Path to the SQLite database file.
    #[arg(long, default_value = "ctxd.db", global = true)]
    db: PathBuf,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the ctxd daemon (HTTP + MCP over stdio + wire protocol).
    Serve {
        /// Address to bind the HTTP admin API.
        #[arg(long, default_value = "127.0.0.1:7777")]
        bind: String,

        /// Address to bind the wire protocol (MessagePack over TCP).
        #[arg(long, default_value = "127.0.0.1:7778")]
        wire_bind: String,

        /// Run MCP server on stdio (for Claude Desktop / mcp-inspector).
        #[arg(long, default_value_t = true)]
        mcp_stdio: bool,
    },

    /// Append an event to the store.
    Write {
        /// Subject path.
        #[arg(long)]
        subject: String,

        /// Event type.
        #[arg(long, rename_all = "verbatim")]
        r#type: String,

        /// Event data as JSON string.
        #[arg(long)]
        data: String,

        /// Sign the event with the stored Ed25519 signing key.
        #[arg(long, default_value_t = false)]
        sign: bool,
    },

    /// Read events for a subject.
    Read {
        /// Subject path.
        #[arg(long)]
        subject: String,

        /// Read recursively under the subject.
        #[arg(long, default_value_t = false)]
        recursive: bool,
    },

    /// Run an EventQL query against the log.
    Query {
        /// The EventQL query string.
        query: String,
    },

    /// Mint a new capability token.
    Grant {
        /// Subject glob pattern.
        #[arg(long, default_value = "/**")]
        subject: String,

        /// Operations to grant (comma-separated: read,write,subjects,search,admin).
        #[arg(long, default_value = "read,write,subjects,search")]
        operations: String,
    },

    /// Verify a capability token.
    Verify {
        /// Base64-encoded token.
        #[arg(long)]
        token: String,

        /// Subject to verify against.
        #[arg(long)]
        subject: String,

        /// Operation to verify.
        #[arg(long)]
        operation: String,
    },

    /// Revoke a capability token.
    Revoke {
        /// Token ID to revoke.
        #[arg(long)]
        token_id: String,
    },

    /// Verify an event's Ed25519 signature.
    VerifySignature {
        /// Event ID to verify.
        #[arg(long)]
        event_id: String,

        /// Hex-encoded public key (32 bytes = 64 hex chars).
        #[arg(long)]
        public_key: String,
    },

    /// List subjects in the store.
    Subjects {
        /// Optional prefix to filter.
        #[arg(long)]
        prefix: Option<String>,

        /// List recursively.
        #[arg(long, default_value_t = false)]
        recursive: bool,
    },

    /// Connect to a running ctxd daemon via the wire protocol.
    Connect {
        /// Address of the daemon's wire protocol endpoint.
        #[arg(long, default_value = "127.0.0.1:7778")]
        addr: String,

        /// Command to send: ping, pub, query, grant.
        #[command(subcommand)]
        action: ConnectAction,
    },
}

/// Actions available through the wire protocol client.
#[derive(Subcommand)]
enum ConnectAction {
    /// Send a PING to check the daemon is alive.
    Ping,
    /// Publish an event via the wire protocol.
    Pub {
        /// Subject path.
        #[arg(long)]
        subject: String,
        /// Event type.
        #[arg(long, rename_all = "verbatim")]
        r#type: String,
        /// Event data as JSON string.
        #[arg(long)]
        data: String,
    },
    /// Query a materialized view via the wire protocol.
    Query {
        /// Subject pattern.
        #[arg(long)]
        subject: String,
        /// View name: log, kv, or fts.
        #[arg(long, default_value = "log")]
        view: String,
    },
    /// Mint a capability token via the wire protocol.
    Grant {
        /// Subject glob pattern.
        #[arg(long, default_value = "/**")]
        subject: String,
        /// Operations to grant (comma-separated).
        #[arg(long, default_value = "read,write,subjects,search")]
        operations: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing. If OTEL_EXPORTER_OTLP_ENDPOINT is set, add an
    // OpenTelemetry layer so spans are exported to an OTLP-compatible backend.
    // Otherwise, use the plain fmt subscriber with zero OTEL overhead.
    let _otel_guard = init_tracing();

    let cli = Cli::parse();
    let store = EventStore::open(&cli.db)
        .await
        .context("failed to open event store")?;
    // Load or create the root capability key, persisted in the database
    let cap_engine = match store.get_metadata("root_key").await? {
        Some(key_bytes) => {
            Arc::new(CapEngine::from_private_key(&key_bytes).context("invalid stored root key")?)
        }
        None => {
            let engine = CapEngine::new();
            store
                .set_metadata("root_key", &engine.private_key_bytes())
                .await
                .context("failed to persist root key")?;
            Arc::new(engine)
        }
    };

    match cli.command {
        Commands::Serve {
            bind,
            wire_bind,
            mcp_stdio,
        } => {
            let addr: SocketAddr = bind.parse().context("invalid bind address")?;
            let wire_addr: SocketAddr = wire_bind.parse().context("invalid wire bind address")?;
            tracing::info!("starting ctxd daemon on {addr}");

            let router = build_router(store.clone(), cap_engine.clone());
            let http_handle = tokio::spawn(async move {
                let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
                tracing::info!("HTTP admin API listening on {addr}");
                axum::serve(listener, router).await.unwrap();
            });

            // Spawn wire protocol server (MessagePack over TCP).
            let wire_server = ProtocolServer::new(store.clone(), cap_engine.clone(), wire_addr);
            let wire_handle = tokio::spawn(async move {
                if let Err(e) = wire_server.run().await {
                    tracing::error!("Wire protocol server error: {e}");
                }
            });

            if mcp_stdio {
                let mcp_server = CtxdMcpServer::new(store, cap_engine, format!("ctxd://{addr}"));
                tracing::info!("MCP server on stdio ready");
                let transport = rmcp::transport::io::stdio();
                let running = mcp_server
                    .serve(transport)
                    .await
                    .context("MCP server failed to start")?;
                let _ = running.waiting().await;
            } else {
                let _ = tokio::join!(http_handle, wire_handle);
            }
        }

        Commands::Write {
            subject,
            r#type,
            data,
            sign,
        } => {
            let subject = Subject::new(&subject).context("invalid subject")?;
            let data: serde_json::Value =
                serde_json::from_str(&data).context("invalid JSON data")?;
            let mut event = Event::new("ctxd://cli".to_string(), subject, r#type, data);

            if sign {
                // Load or create signing key from metadata
                let signer = match store.get_metadata("signing_key").await? {
                    Some(key_bytes) => EventSigner::from_bytes(&key_bytes)
                        .map_err(|e| anyhow::anyhow!("invalid signing key: {e}"))?,
                    None => {
                        let signer = EventSigner::new();
                        store
                            .set_metadata("signing_key", &signer.secret_key_bytes())
                            .await
                            .context("failed to persist signing key")?;
                        // Also store the public key for later verification
                        store
                            .set_metadata("signing_public_key", &signer.public_key_bytes())
                            .await
                            .context("failed to persist signing public key")?;
                        signer
                    }
                };
                event.signature = Some(signer.sign(&event).context("failed to sign event")?);
            }

            let stored = store.append(event).await.context("write failed")?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "id": stored.id.to_string(),
                    "subject": stored.subject.as_str(),
                    "predecessorhash": stored.predecessorhash,
                    "signature": stored.signature,
                }))?
            );
        }

        Commands::Read { subject, recursive } => {
            let subject = Subject::new(&subject).context("invalid subject")?;
            let events = store
                .read(&subject, recursive)
                .await
                .context("read failed")?;
            let output: Vec<serde_json::Value> = events
                .iter()
                .map(|e| serde_json::to_value(e).unwrap())
                .collect();
            println!("{}", serde_json::to_string_pretty(&output)?);
        }

        Commands::Query { query: query_str } => match query::parse_query(&query_str) {
            Ok(parsed) => match query::execute_query(&store, &parsed).await {
                Ok(events) => {
                    let output: Vec<serde_json::Value> = events
                        .iter()
                        .map(|e| serde_json::to_value(e).unwrap())
                        .collect();
                    println!("{}", serde_json::to_string_pretty(&output)?);
                }
                Err(e) => {
                    eprintln!("Query execution failed: {e}");
                }
            },
            Err(e) => {
                eprintln!("EventQL parse error: {e}");
                eprintln!("Supported: FROM e IN events WHERE e.subject LIKE \"/pattern/%\" AND e.type = \"ctx.note\" AND e.time > \"2025-01-01T00:00:00Z\" PROJECT INTO e");
            }
        },

        Commands::Grant {
            subject,
            operations,
        } => {
            let ops: Result<Vec<Operation>, _> = operations
                .split(',')
                .map(|op| match op.trim() {
                    "read" => Ok(Operation::Read),
                    "write" => Ok(Operation::Write),
                    "subjects" => Ok(Operation::Subjects),
                    "search" => Ok(Operation::Search),
                    "admin" => Ok(Operation::Admin),
                    other => anyhow::bail!("unknown operation: {other}"),
                })
                .collect();
            let ops = ops?;
            let token = cap_engine
                .mint(&subject, &ops, None, None, None)
                .context("mint failed")?;
            println!("{}", CapEngine::token_to_base64(&token));
        }

        Commands::Verify {
            token,
            subject,
            operation,
        } => {
            let op = match operation.as_str() {
                "read" => Operation::Read,
                "write" => Operation::Write,
                "subjects" => Operation::Subjects,
                "search" => Operation::Search,
                "admin" => Operation::Admin,
                other => anyhow::bail!("unknown operation: {other}"),
            };
            let bytes = CapEngine::token_from_base64(&token).context("invalid token")?;
            match cap_engine.verify(&bytes, &subject, op, None) {
                Ok(()) => println!("VERIFIED: token is valid for {operation} on {subject}"),
                Err(e) => println!("DENIED: {e}"),
            }
        }

        Commands::Revoke { token_id } => {
            store
                .revoke_token(&token_id)
                .await
                .context("revoke failed")?;
            println!("Token {token_id} has been revoked.");
        }

        Commands::VerifySignature {
            event_id,
            public_key,
        } => {
            let pk_bytes = hex::decode(&public_key).context("invalid hex public key")?;
            // Find the event by ID
            let subject = Subject::new("/").unwrap();
            let all_events = store.read(&subject, true).await.context("read failed")?;
            let event = all_events
                .iter()
                .find(|e| e.id.to_string() == event_id)
                .ok_or_else(|| anyhow::anyhow!("event not found: {event_id}"))?;
            match &event.signature {
                Some(sig) => {
                    if EventSigner::verify(event, sig, &pk_bytes) {
                        println!("VERIFIED: signature is valid.");
                    } else {
                        println!("INVALID: signature verification failed.");
                    }
                }
                None => {
                    println!("Event has no signature.");
                }
            }
        }

        Commands::Subjects { prefix, recursive } => {
            let prefix = prefix
                .as_deref()
                .map(Subject::new)
                .transpose()
                .context("invalid prefix")?;
            let subjects = store
                .subjects(prefix.as_ref(), recursive)
                .await
                .context("subjects failed")?;
            println!("{}", serde_json::to_string_pretty(&subjects)?);
        }

        Commands::Connect { addr, action } => {
            let mut client = ProtocolClient::connect(&addr)
                .await
                .context("failed to connect to daemon")?;

            match action {
                ConnectAction::Ping => {
                    client.ping().await.context("ping failed")?;
                    println!("PONG — daemon is alive");
                }
                ConnectAction::Pub {
                    subject,
                    r#type,
                    data,
                } => {
                    let data: serde_json::Value =
                        serde_json::from_str(&data).context("invalid JSON data")?;
                    let response = client
                        .publish(&subject, &r#type, data)
                        .await
                        .context("publish failed")?;
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::to_value(&response)?)?
                    );
                }
                ConnectAction::Query { subject, view } => {
                    let response = client
                        .query(&subject, &view)
                        .await
                        .context("query failed")?;
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::to_value(&response)?)?
                    );
                }
                ConnectAction::Grant {
                    subject,
                    operations,
                } => {
                    let ops: Vec<&str> = operations.split(',').map(|s| s.trim()).collect();
                    let response = client
                        .grant(&subject, &ops, None)
                        .await
                        .context("grant failed")?;
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::to_value(&response)?)?
                    );
                }
            }
        }
    }

    Ok(())
}

/// Guard that shuts down the OpenTelemetry tracer provider on drop.
/// When OTEL is not enabled, this is a no-op.
struct OtelGuard {
    provider: Option<opentelemetry_sdk::trace::SdkTracerProvider>,
}

impl Drop for OtelGuard {
    fn drop(&mut self) {
        if let Some(provider) = self.provider.take() {
            if let Err(e) = provider.shutdown() {
                eprintln!("OpenTelemetry shutdown error: {e}");
            }
        }
    }
}

/// Initialize the tracing subscriber. If `OTEL_EXPORTER_OTLP_ENDPOINT` is set,
/// an OpenTelemetry tracing layer is added that exports spans over OTLP/gRPC.
/// Otherwise only the plain fmt layer is used (zero OTEL overhead).
fn init_tracing() -> OtelGuard {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let fmt_layer = tracing_subscriber::fmt::layer();

    if std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").is_ok() {
        // Build OTLP exporter and tracer provider.
        let exporter = match opentelemetry_otlp::SpanExporter::builder()
            .with_tonic()
            .build()
        {
            Ok(exp) => exp,
            Err(e) => {
                eprintln!("Failed to create OTLP exporter: {e}. Falling back to fmt-only.");
                tracing_subscriber::registry()
                    .with(env_filter)
                    .with(fmt_layer)
                    .init();
                return OtelGuard { provider: None };
            }
        };

        let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
            .with_batch_exporter(exporter)
            .build();

        let tracer = provider.tracer("ctxd");
        let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);

        tracing_subscriber::registry()
            .with(env_filter)
            .with(fmt_layer)
            .with(otel_layer)
            .init();

        OtelGuard {
            provider: Some(provider),
        }
    } else {
        tracing_subscriber::registry()
            .with(env_filter)
            .with(fmt_layer)
            .init();

        OtelGuard { provider: None }
    }
}
