//! ctxd — context substrate daemon for AI agents.

mod protocol;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use ctxd_cap::{CapEngine, Operation};
use ctxd_core::event::Event;
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
        /// Base64-encoded token to revoke.
        #[arg(long)]
        token: String,
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
        } => {
            let subject = Subject::new(&subject).context("invalid subject")?;
            let data: serde_json::Value =
                serde_json::from_str(&data).context("invalid JSON data")?;
            let event = Event::new("ctxd://cli".to_string(), subject, r#type, data);
            let stored = store.append(event).await.context("write failed")?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "id": stored.id.to_string(),
                    "subject": stored.subject.as_str(),
                    "predecessorhash": stored.predecessorhash,
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

        Commands::Query { query } => {
            // TODO(v0.2): Full EventQL parser integration.
            // EventQL supports: FROM e IN events WHERE e.subject LIKE "/test/%" PROJECT INTO e
            // For v0.1, we parse a basic LIKE filter as a subset.
            eprintln!("EventQL query engine is a v0.2 feature.");
            eprintln!("Running basic subject LIKE filter as fallback...");

            if let Some(pattern) = extract_like_pattern(&query) {
                let all_subjects = store
                    .subjects(None, false)
                    .await
                    .context("subjects failed")?;
                let matching: Vec<&String> = all_subjects
                    .iter()
                    .filter(|s| sql_like_match(s, &pattern))
                    .collect();

                for subj_str in matching {
                    let subj = Subject::new(subj_str).context("invalid subject in store")?;
                    let events = store.read(&subj, false).await.context("read failed")?;
                    for event in &events {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&serde_json::to_value(event)?)?
                        );
                    }
                }
            } else {
                eprintln!("Could not parse query. Supported: FROM e IN events WHERE e.subject LIKE \"<pattern>\" PROJECT INTO e");
            }
        }

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

        Commands::Revoke { token: _ } => {
            println!(
                "Token revocation is a v0.2 feature. Tokens expire based on their expiry caveat."
            );
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

/// Extract a LIKE pattern from a basic EventQL query.
fn extract_like_pattern(query: &str) -> Option<String> {
    let upper = query.to_uppercase();
    let like_pos = upper.find("LIKE")?;
    let rest = &query[like_pos + 4..];
    let rest = rest.trim();
    let start_quote = rest.find('"').or_else(|| rest.find('\''))?;
    let quote_char = rest.as_bytes()[start_quote] as char;
    let after_quote = &rest[start_quote + 1..];
    let end_quote = after_quote.find(quote_char)?;
    Some(after_quote[..end_quote].to_string())
}

/// Basic SQL LIKE matching (supports % wildcard).
fn sql_like_match(value: &str, pattern: &str) -> bool {
    if pattern == "%" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix('%') {
        return value.starts_with(prefix);
    }
    if let Some(suffix) = pattern.strip_prefix('%') {
        return value.ends_with(suffix);
    }
    if pattern.contains('%') {
        let parts: Vec<&str> = pattern.split('%').collect();
        if parts.len() == 2 {
            return value.starts_with(parts[0]) && value.ends_with(parts[1]);
        }
    }
    value == pattern
}
