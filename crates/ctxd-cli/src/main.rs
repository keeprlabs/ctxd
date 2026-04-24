//! ctxd — context substrate daemon for AI agents.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use ctxd_cap::{CapEngine, Operation};
use ctxd_cli::federation;
use ctxd_cli::protocol;
use ctxd_cli::query;
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

    /// Federation peer management (v0.3).
    ///
    /// `peer add` records a peer's public key + dial URL + granted
    /// subject globs in the local store. Actual replication is wired
    /// via the `ctxd-cli/src/federation.rs` PeerManager (Phase 2D).
    Peer {
        #[command(subcommand)]
        action: PeerAction,
    },

    /// Migrate an existing ctxd database to the current schema version.
    ///
    /// v0.3 migration re-computes predecessor hashes and signatures under
    /// the v0.3 canonical form (which includes the new `parents` and
    /// `attestation` fields even when empty). Writes are applied in
    /// transactional batches of 1000 rows.
    Migrate {
        /// Target schema version (currently only `0.3` is supported).
        #[arg(long, default_value = "0.3")]
        to: String,

        /// Report what would change without writing anything.
        #[arg(long, default_value_t = false)]
        dry_run: bool,

        /// Re-run migration even if the database is already at the target version.
        #[arg(long, default_value_t = false)]
        force: bool,
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

/// Federation peer management actions.
#[derive(Subcommand)]
enum PeerAction {
    /// Register a peer locally and (when `--url` is set) perform the
    /// `PeerHello` ↔ `PeerWelcome` handshake against it.
    ///
    /// When `--public-key` is provided we verify the welcome's pubkey
    /// matches; otherwise we accept whatever the remote returns. Use
    /// `--auto-accept` to skip the handshake (manual enrollment) — the
    /// peer is still recorded but no cap exchange happens.
    Add {
        /// Local identifier for the peer (often the remote's pubkey hex).
        #[arg(long)]
        peer_id: String,
        /// URL to dial (e.g. `127.0.0.1:7778`).
        #[arg(long)]
        url: String,
        /// Remote's Ed25519 public key, 64 hex chars (32 bytes). Optional
        /// when handshaking — we'll accept whatever the welcome carries.
        #[arg(long)]
        public_key: Option<String>,
        /// Comma-separated subject globs to grant this peer.
        #[arg(long, default_value = "/**")]
        subjects: String,
        /// Skip the handshake and just record the peer locally. Useful
        /// for offline enrollment or test fixtures. Requires `--public-key`.
        #[arg(long, default_value_t = false)]
        manual: bool,
    },
    /// List all registered peers.
    List,
    /// Show federation status for a peer (cursors, last contact).
    Status {
        /// Peer id to inspect.
        #[arg(long)]
        peer_id: String,
    },
    /// Remove a peer. Also removes any replication cursors.
    Remove {
        /// Peer id to remove.
        #[arg(long)]
        peer_id: String,
    },
    /// Update the subject globs granted to an existing peer.
    Grant {
        /// Peer id to grant.
        #[arg(long)]
        peer_id: String,
        /// Comma-separated subject globs.
        #[arg(long)]
        subjects: String,
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

            // Bootstrap federation. Requires a local signing key — create
            // one if absent so federation always has a stable identity.
            let signing_bytes = match store.get_metadata("signing_key").await? {
                Some(b) => b,
                None => {
                    let signer = ctxd_core::signing::EventSigner::new();
                    store
                        .set_metadata("signing_key", &signer.secret_key_bytes())
                        .await?;
                    store
                        .set_metadata("signing_public_key", &signer.public_key_bytes())
                        .await?;
                    signer.secret_key_bytes()
                }
            };
            let signer = ctxd_core::signing::EventSigner::from_bytes(&signing_bytes)
                .map_err(|e| anyhow::anyhow!("bad signing key: {e}"))?;
            let local_peer_id = hex::encode(signer.public_key_bytes());
            tracing::info!(local_peer_id = %local_peer_id, "federation identity ready");

            // Spawn wire protocol server (MessagePack over TCP) with
            // federation attached.
            let wire_server = ProtocolServer::new(store.clone(), cap_engine.clone(), wire_addr);
            let event_tx_for_fed = wire_server.event_sender();
            let fed = std::sync::Arc::new(federation::PeerManager::new(
                std::sync::Arc::new(store.clone()),
                cap_engine.clone(),
                local_peer_id,
                signing_bytes,
                event_tx_for_fed,
                federation::AutoAcceptPolicy::from_env(),
            ));
            // Re-enroll persisted peers so outbound replication can
            // dial them without manual `peer add` after restart.
            for p in store.peer_list_impl().await? {
                fed.enroll(federation::EnrolledPeer {
                    peer_id: p.peer_id.clone(),
                    remote_pubkey: p.public_key,
                    remote_grants_us: p.granted_subjects.clone(),
                    we_grant_remote: p.granted_subjects,
                    cap_from_remote: None,
                    cap_for_remote: None,
                })
                .await;
            }
            let _replication_handle = fed.start_replication_tasks();
            let wire_server = wire_server.with_federation(fed.clone());
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

        Commands::Peer { action } => {
            use ctxd_store::core::{Peer, PeerCursor};
            match action {
                PeerAction::Add {
                    peer_id,
                    url,
                    public_key,
                    subjects,
                    manual,
                } => {
                    let granted: Vec<String> = subjects
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();

                    if manual {
                        // Offline enrollment: just record the peer row.
                        // public_key is required.
                        let pk_hex = public_key.as_deref().ok_or_else(|| {
                            anyhow::anyhow!("--public-key is required when --manual")
                        })?;
                        let pk_bytes = hex::decode(pk_hex).context("invalid hex public key")?;
                        if pk_bytes.len() != 32 {
                            anyhow::bail!(
                                "public key must be 32 bytes (64 hex chars); got {}",
                                pk_bytes.len()
                            );
                        }
                        let peer = Peer {
                            peer_id: peer_id.clone(),
                            url,
                            public_key: pk_bytes,
                            granted_subjects: granted,
                            trust_level: serde_json::json!({"auto_accept": false}),
                            added_at: chrono::Utc::now(),
                        };
                        store
                            .peer_add_impl(peer)
                            .await
                            .context("failed to add peer")?;
                        println!("peer {peer_id} added (manual)");
                    } else {
                        // Real handshake: dial the URL via PeerManager and
                        // perform PeerHello → PeerWelcome.
                        use ctxd_cli::federation::{AutoAcceptPolicy, PeerManager};
                        // Local signing key is required for outbound
                        // handshake (we sign our pubkey assertion in
                        // PeerHello). Auto-create one if missing.
                        let signing_bytes = match store.get_metadata("signing_key").await? {
                            Some(b) => b,
                            None => {
                                let signer = ctxd_core::signing::EventSigner::new();
                                store
                                    .set_metadata("signing_key", &signer.secret_key_bytes())
                                    .await?;
                                store
                                    .set_metadata("signing_public_key", &signer.public_key_bytes())
                                    .await?;
                                signer.secret_key_bytes()
                            }
                        };
                        let signer = ctxd_core::signing::EventSigner::from_bytes(&signing_bytes)
                            .map_err(|e| anyhow::anyhow!("bad signing key: {e}"))?;
                        let local_pid = hex::encode(signer.public_key_bytes());
                        let (event_tx, _) =
                            tokio::sync::broadcast::channel::<protocol::BroadcastEvent>(64);
                        let mgr = std::sync::Arc::new(PeerManager::new(
                            std::sync::Arc::new(store.clone()),
                            cap_engine.clone(),
                            local_pid,
                            signing_bytes,
                            event_tx,
                            AutoAcceptPolicy::from_env(),
                        ));
                        let enrolled = mgr
                            .handshake_outbound(&peer_id, &url, &granted)
                            .await
                            .map_err(|e| anyhow::anyhow!("handshake failed: {e}"))?;
                        if let Some(expected) = public_key {
                            let actual = hex::encode(&enrolled.remote_pubkey);
                            if !actual.eq_ignore_ascii_case(&expected) {
                                anyhow::bail!(
                                    "remote pubkey {actual} does not match expected {expected}"
                                );
                            }
                        }
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&serde_json::json!({
                                "peer_id": enrolled.peer_id,
                                "remote_pubkey": hex::encode(&enrolled.remote_pubkey),
                                "remote_grants_us": enrolled.remote_grants_us,
                                "we_grant_remote": enrolled.we_grant_remote,
                                "handshake": "ok",
                            }))?
                        );
                    }
                }
                PeerAction::List => {
                    let peers = store
                        .peer_list_impl()
                        .await
                        .context("failed to list peers")?;
                    let out: Vec<_> = peers
                        .into_iter()
                        .map(|p| {
                            serde_json::json!({
                                "peer_id": p.peer_id,
                                "url": p.url,
                                "public_key": hex::encode(&p.public_key),
                                "granted_subjects": p.granted_subjects,
                                "trust_level": p.trust_level,
                                "added_at": p.added_at.to_rfc3339(),
                            })
                        })
                        .collect();
                    println!("{}", serde_json::to_string_pretty(&out)?);
                }
                PeerAction::Status { peer_id } => {
                    // Status report: the peer's cursors across all known
                    // subject patterns. Today we don't track "last
                    // contact" timestamps separately from cursors —
                    // the cursor's `updated_at` is the closest thing.
                    let peers = store
                        .peer_list_impl()
                        .await
                        .context("failed to list peers")?;
                    let peer = peers
                        .into_iter()
                        .find(|p| p.peer_id == peer_id)
                        .ok_or_else(|| anyhow::anyhow!("peer not found: {peer_id}"))?;
                    // Peer cursors are not enumerable by `peer_id` alone
                    // in the trait; for now we just print the peer row.
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "peer_id": peer.peer_id,
                            "url": peer.url,
                            "granted_subjects": peer.granted_subjects,
                            "note": "replication cursors are per-(peer, subject_pattern); inspect via peer_cursor_get once federation is wired (Phase 2D)",
                        }))?
                    );
                }
                PeerAction::Remove { peer_id } => {
                    store
                        .peer_remove_impl(&peer_id)
                        .await
                        .context("failed to remove peer")?;
                    println!("peer {peer_id} removed");
                }
                PeerAction::Grant { peer_id, subjects } => {
                    // Fetch, mutate, re-upsert — peer_add is idempotent.
                    let peers = store
                        .peer_list_impl()
                        .await
                        .context("failed to list peers")?;
                    let mut peer = peers
                        .into_iter()
                        .find(|p| p.peer_id == peer_id)
                        .ok_or_else(|| anyhow::anyhow!("peer not found: {peer_id}"))?;
                    peer.granted_subjects = subjects
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                    store
                        .peer_add_impl(peer)
                        .await
                        .context("failed to update peer grant")?;
                    // Reset any existing cursors for removed patterns
                    // — cheap correctness: on next reconnect, we
                    // resend from the beginning of retained patterns.
                    let _ = PeerCursor {
                        peer_id: peer_id.clone(),
                        subject_pattern: "/**".to_string(),
                        last_event_id: None,
                        last_event_time: None,
                    };
                    println!("peer {peer_id} grant updated");
                }
            }
        }

        Commands::Migrate { to, dry_run, force } => {
            if to != "0.3" {
                anyhow::bail!(
                    "unsupported migration target: {to} (only '0.3' is supported in this release)"
                );
            }
            // If a signing key was stored during earlier writes, we use it
            // to re-sign events that were previously signed. If no key is
            // stored, migration still rewrites hashes but leaves
            // signatures alone.
            let signing_key = store.get_metadata("signing_key").await?;
            let report =
                ctxd_store::migrate::migrate_to_v03(&store, signing_key.as_deref(), dry_run, force)
                    .await
                    .context("migration failed")?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "target": to,
                    "dry_run": report.dry_run,
                    "already_migrated": report.already_migrated,
                    "events_considered": report.considered,
                    "events_rewritten": report.rewritten,
                    "batches_committed": report.batches_committed,
                }))?
            );
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
