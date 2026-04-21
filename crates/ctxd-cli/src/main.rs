//! ctxd — context substrate daemon for AI agents.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use ctxd_cap::{CapEngine, Operation};
use ctxd_core::event::Event;
use ctxd_core::subject::Subject;
use ctxd_http::build_router;
use ctxd_mcp::CtxdMcpServer;
use ctxd_store::EventStore;
use rmcp::ServiceExt;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
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
    /// Start the ctxd daemon (HTTP + MCP over stdio).
    Serve {
        /// Address to bind the HTTP admin API.
        #[arg(long, default_value = "127.0.0.1:7777")]
        bind: String,

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

    /// List subjects in the store.
    Subjects {
        /// Optional prefix to filter.
        #[arg(long)]
        prefix: Option<String>,

        /// List recursively.
        #[arg(long, default_value_t = false)]
        recursive: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    let store = EventStore::open(&cli.db)
        .await
        .context("failed to open event store")?;
    // Load or create the root capability key, persisted in the database
    let cap_engine = match store.get_metadata("root_key").await? {
        Some(key_bytes) => Arc::new(
            CapEngine::from_private_key(&key_bytes).context("invalid stored root key")?,
        ),
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
        Commands::Serve { bind, mcp_stdio } => {
            let addr: SocketAddr = bind.parse().context("invalid bind address")?;
            tracing::info!("starting ctxd daemon on {addr}");

            let router = build_router(store.clone(), cap_engine.clone());
            let http_handle = tokio::spawn(async move {
                let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
                tracing::info!("HTTP admin API listening on {addr}");
                axum::serve(listener, router).await.unwrap();
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
                http_handle.await.context("HTTP server failed")?;
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
                .mint(&subject, &ops, None)
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
            match cap_engine.verify(&bytes, &subject, op) {
                Ok(()) => println!("VERIFIED: token is valid for {operation} on {subject}"),
                Err(e) => println!("DENIED: {e}"),
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
    }

    Ok(())
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
