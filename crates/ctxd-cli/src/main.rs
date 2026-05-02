//! ctxd — context substrate daemon for AI agents.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use ctxd_cap::state::{ApprovalDecision, CaveatState, PendingApproval};
use ctxd_cap::{CapEngine, Operation};
use ctxd_cli::protocol;
use ctxd_cli::query;
use ctxd_core::event::Event;
use ctxd_core::signing::EventSigner;
use ctxd_core::subject::Subject;
use ctxd_store::caveat_state::SqliteCaveatState;
use ctxd_store::EventStore;
use opentelemetry::trace::TracerProvider;
use protocol::ProtocolClient;
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
    /// Start the ctxd daemon (HTTP admin + wire protocol + MCP transports).
    ///
    /// MCP can be served over up to three transports concurrently:
    ///
    /// * `--mcp-stdio` — newline-delimited JSON-RPC on stdin/stdout
    ///   (default on; the only transport Claude Desktop currently uses).
    /// * `--mcp-sse <addr>` — legacy MCP SSE: `GET /sse` opens an event
    ///   stream, `POST /messages?sessionId=…` carries JSON-RPC.
    /// * `--mcp-http <addr>` — modern streamable HTTP: a single `/mcp`
    ///   endpoint per the MCP 2025-03-26 spec.
    ///
    /// On the HTTP transports, capability tokens may be presented as
    /// either an `Authorization: Bearer <base64-biscuit>` header or a
    /// per-call `token` argument. The header wins when both are
    /// present. With `--require-auth`, every `tools/call` over the
    /// HTTP transports must present a token (header or arg) — calls
    /// without one are rejected with 401.
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

        /// Bind a legacy MCP SSE transport at this address (e.g.
        /// `127.0.0.1:7779`). Disabled by default.
        #[arg(long)]
        mcp_sse: Option<String>,

        /// Bind a streamable-HTTP MCP transport at this address (e.g.
        /// `127.0.0.1:7780`). Disabled by default.
        #[arg(long)]
        mcp_http: Option<String>,

        /// Require every `tools/call` on the HTTP transports to carry
        /// a capability token. Calls without one return 401. Stdio is
        /// unaffected — that transport keeps the legacy "open by
        /// default" behaviour for local-subprocess clients.
        #[arg(long, default_value_t = false)]
        require_auth: bool,

        /// Embedder backend: `null` (default — zero vectors),
        /// `openai`, or `ollama`. Real backends require the
        /// matching feature flag at compile time.
        #[arg(long = "embedder", default_value = "null")]
        embedder: String,

        /// Override the embedder model. Provider-specific defaults
        /// apply when unset (`text-embedding-3-small` for OpenAI,
        /// `nomic-embed-text` for Ollama).
        #[arg(long = "embedder-model")]
        embedder_model: Option<String>,

        /// Override the embedder base URL.
        #[arg(long = "embedder-url")]
        embedder_url: Option<String>,

        /// API key for the embedder (OpenAI only). Falls back to
        /// `OPENAI_API_KEY` env if unset. Never logged.
        #[arg(long = "embedder-api-key")]
        embedder_api_key: Option<String>,

        /// Storage backend: `sqlite` (default), `postgres`, or
        /// `duckdb-object`. The non-default backends require their
        /// matching Cargo feature (`storage-postgres`,
        /// `storage-duckdb-object`). For backwards-compat, the
        /// always-on baseline path keeps using `--db <file>` for
        /// SQLite; `--storage-uri` is consumed for non-default kinds.
        #[arg(long = "storage", default_value = "sqlite")]
        storage: String,

        /// URI for the selected backend. `postgres://...` for the
        /// Postgres backend, `file:///abs/path` (or a bare path) for
        /// the duckdb-object backend's local-fs mode.
        #[arg(long = "storage-uri")]
        storage_uri: Option<String>,
    },

    /// Open the embedded web dashboard at `http://127.0.0.1:7777/`.
    ///
    /// Starts an HTTP-only daemon (no wire, no MCP, no federation)
    /// against the same SQLite database `ctxd serve` uses, then opens
    /// the URL in the system browser. Read-only by default — writes
    /// still go through MCP, the wire protocol, or the CLI.
    Dashboard {
        /// Address to bind the dashboard's HTTP server.
        #[arg(long, default_value = "127.0.0.1:7777")]
        bind: String,

        /// Skip opening a browser. Useful in CI or over SSH.
        #[arg(long, default_value_t = false)]
        no_open: bool,
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

    /// Decide a pending human-approval request (v0.3 `HumanApprovalRequired`).
    ///
    /// Reads the approval row from the database and updates it. The
    /// daemon's verifier task wakes up on the next poll/notify pass.
    Approve {
        /// Approval id (the UUIDv7 the daemon emitted at request time).
        #[arg(long)]
        id: String,
        /// Decision to record: `allow` or `deny`.
        #[arg(long)]
        decision: String,
    },

    /// Set up ctxd as a one-time install: service + clients + caps + seeds.
    ///
    /// The single command that turns a fresh `ctxd` install into a
    /// running, MCP-connected, opinion-having context substrate.
    /// Walks the seven onboarding steps, optionally pausing for
    /// adapter consent. See `docs/onboarding.md` for the full
    /// step-by-step.
    ///
    /// `--skill-mode` switches output to newline-delimited JSON per
    /// the `docs/onboard-protocol.md` contract — the Claude Code
    /// skill (in `skill/ctxd-memory/`) and any other front door
    /// shell to ctxd in this mode.
    Onboard {
        /// Output as newline-delimited JSON per `docs/onboard-protocol.md`.
        /// Implies `--headless`.
        #[arg(long, default_value_t = false)]
        skill_mode: bool,

        /// Run with all defaults; never pause on a prompt.
        #[arg(long, default_value_t = false)]
        headless: bool,

        /// Plan only — emit step messages but make no changes.
        #[arg(long, default_value_t = false)]
        dry_run: bool,

        /// Skip the configure-adapters step entirely.
        #[arg(long, default_value_t = false)]
        skip_adapters: bool,

        /// Don't install the system service. Useful when running
        /// `ctxd serve` in a foreground terminal instead.
        #[arg(long, default_value_t = false)]
        skip_service: bool,

        /// Configure the service to start at user login.
        #[arg(long, default_value_t = false)]
        at_login: bool,

        /// Mint narrower per-client capability tokens (phase 2A).
        #[arg(long, default_value_t = false)]
        strict_scopes: bool,

        /// Write Claude Code SessionStart / UserPromptSubmit /
        /// PreCompact / Stop hooks (phase 2B).
        #[arg(long, default_value_t = true)]
        with_hooks: bool,

        /// Comma-separated list of step slugs to run (e.g.
        /// `service-install,service-start`). Default: all steps.
        #[arg(long)]
        only: Option<String>,

        /// Address to bind the daemon's HTTP admin API.
        #[arg(long, default_value = "127.0.0.1:7777")]
        bind: String,

        /// Address to bind the wire protocol.
        #[arg(long, default_value = "127.0.0.1:7778")]
        wire_bind: String,
    },

    /// Reverse a previous `ctxd onboard` cleanly.
    ///
    /// Stops + uninstalls the system service, deletes capability
    /// files, and (with `--purge`) removes the SQLite DB. Idempotent
    /// — running offboard on a clean system is a no-op.
    Offboard {
        /// Output as newline-delimited JSON.
        #[arg(long, default_value_t = false)]
        skill_mode: bool,

        /// Plan only — emit messages but make no changes.
        #[arg(long, default_value_t = false)]
        dry_run: bool,

        /// Also delete the SQLite DB and HNSW sidecars. Default off
        /// — caller must opt in to data loss.
        #[arg(long, default_value_t = false)]
        purge: bool,

        /// Don't touch the system service. Useful if you want to
        /// keep launchd / systemd config but delete data.
        #[arg(long, default_value_t = false)]
        skip_service: bool,
    },

    /// Run diagnostic checks on the local ctxd installation.
    ///
    /// Reports daemon health, storage integrity, configured clients,
    /// minted capabilities, and adapter status. Each failed check
    /// includes a remediation hint. Run anytime — standalone, or as
    /// step 7 of `ctxd onboard`.
    Doctor {
        /// Emit JSON instead of human-readable output. Used by the
        /// Claude Code skill and CI scripts. Schema mirrors
        /// `onboard::doctor::Check`.
        #[arg(long, default_value_t = false)]
        json: bool,
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

    // Build the v0.3 stateful-caveat backing once and share it across
    // every transport that needs to enforce budgets / approvals.
    // SQLite-backed so values survive a restart.
    let caveat_state: Arc<dyn CaveatState> = Arc::new(SqliteCaveatState::new(store.clone()));

    // PendingApproval broadcast: any task that emits a new approval
    // request can publish here so a future notifier adapter (Slack,
    // email) can subscribe. The daemon itself does not consume the
    // channel — it's a side-channel for adapters.
    let (pending_approval_tx, _pending_approval_rx) =
        tokio::sync::broadcast::channel::<PendingApproval>(64);

    match cli.command {
        Commands::Serve {
            bind,
            wire_bind,
            mcp_stdio,
            mcp_sse,
            mcp_http,
            require_auth,
            embedder,
            embedder_model,
            embedder_url,
            embedder_api_key,
            storage,
            storage_uri,
        } => {
            let cfg = ctxd_cli::serve::ServeConfig {
                bind,
                wire_bind: Some(wire_bind),
                mcp_stdio,
                mcp_sse,
                mcp_http,
                require_auth,
                embedder,
                embedder_model,
                embedder_url,
                embedder_api_key,
                storage,
                storage_uri,
                federation: true,
                db_path: Some(cli.db.clone()),
            };
            ctxd_cli::serve::serve(cfg, store, cap_engine, caveat_state, pending_approval_tx)
                .await?;
        }

        Commands::Dashboard { bind, no_open } => {
            // Dashboard mode: HTTP only. No wire, no MCP, no
            // federation. Reuses serve()'s composition (HTTP admin +
            // dashboard frontend + loopback middleware) — the
            // refactor in step 0 made this a four-line subcommand.
            let cfg = ctxd_cli::serve::ServeConfig {
                bind: bind.clone(),
                wire_bind: None,
                mcp_stdio: false,
                mcp_sse: None,
                mcp_http: None,
                require_auth: false,
                embedder: "null".to_string(),
                embedder_model: None,
                embedder_url: None,
                embedder_api_key: None,
                storage: "sqlite".to_string(),
                storage_uri: None,
                federation: false,
                db_path: Some(cli.db.clone()),
            };
            // Spawn a deferred opener that fires once the bind has
            // had a moment to come up. Doing this *before* the
            // serve() future is awaited would race; doing it *after*
            // would never fire (serve runs forever). Tokio task it.
            if !no_open {
                let url = format!("http://{}/", bind);
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                    if let Err(e) = open_browser(&url) {
                        tracing::warn!(error = %e, "couldn't open browser; visit {url} manually");
                    } else {
                        tracing::info!("opened dashboard at {url}");
                    }
                });
            } else {
                tracing::info!("ctxd dashboard listening on http://{bind}/");
            }
            // Run the serve loop. EADDRINUSE etc. surface as anyhow
            // errors with a friendly hint when the bind fails.
            ctxd_cli::serve::serve(cfg, store, cap_engine, caveat_state, pending_approval_tx)
                .await
                .map_err(|e| {
                    // anyhow's Display only shows the top context; the
                    // underlying io::Error lives in the chain. Walk it so
                    // we can surface a friendly message for the common
                    // "another daemon is already running on this port"
                    // case.
                    let in_use = e.chain().any(|cause| {
                        let s = cause.to_string();
                        s.contains("Address already in use") || s.contains("address in use")
                    });
                    if in_use {
                        anyhow::anyhow!(
                            "port {} is already in use. a ctxd daemon may already \
                         be running — visit http://{}/ in your browser, or \
                         stop the existing daemon and try again.",
                            bind,
                            bind
                        )
                    } else {
                        e
                    }
                })?;
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

        Commands::Approve { id, decision } => {
            let dec = match decision.to_ascii_lowercase().as_str() {
                "allow" => ApprovalDecision::Allow,
                "deny" => ApprovalDecision::Deny,
                other => anyhow::bail!("--decision must be 'allow' or 'deny', got '{other}'"),
            };
            // The CLI talks to the daemon's database directly: it
            // doesn't make sense to require the daemon be running
            // (ops scenario: emergency deny while serve is down).
            caveat_state
                .approval_decide(&id, dec)
                .await
                .context("failed to record decision")?;
            tracing::info!(approval_id = %id, decision = ?dec, "approval decided via CLI");
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "approval_id": id,
                    "decision": match dec {
                        ApprovalDecision::Allow => "allow",
                        ApprovalDecision::Deny => "deny",
                        ApprovalDecision::Pending => "pending",
                    },
                    "status": "ok",
                }))?
            );
        }

        Commands::Onboard {
            skill_mode,
            headless,
            dry_run,
            skip_adapters,
            skip_service,
            at_login,
            strict_scopes,
            with_hooks,
            only,
            bind,
            wire_bind,
        } => {
            use ctxd_cli::onboard::pipeline::{onboard, AdapterChoice, PipelineConfig};
            use ctxd_cli::onboard::protocol::OutputMode;
            use std::collections::HashSet;
            let mode = if skill_mode {
                OutputMode::Skill
            } else {
                OutputMode::Human
            };
            let only_set = only.as_deref().map(|s| {
                s.split(',')
                    .map(|p| p.trim())
                    .filter(|p| !p.is_empty())
                    .filter_map(parse_step)
                    .collect::<HashSet<_>>()
            });
            let cfg = PipelineConfig {
                mode,
                headless: headless || skill_mode,
                dry_run,
                skip_adapters,
                skip_service,
                at_login,
                strict_scopes,
                with_hooks,
                gmail: AdapterChoice::Skip,
                github: AdapterChoice::Skip,
                fs: vec![],
                only: only_set,
                db_path: cli.db.clone(),
                bind,
                wire_bind,
            };
            let outcome = onboard(cfg).await?;
            if !outcome.onboarded {
                std::process::exit(1);
            }
        }

        Commands::Offboard {
            skill_mode,
            dry_run,
            purge,
            skip_service,
        } => {
            use ctxd_cli::onboard::pipeline::{offboard, AdapterChoice, PipelineConfig};
            use ctxd_cli::onboard::protocol::OutputMode;
            let mode = if skill_mode {
                OutputMode::Skill
            } else {
                OutputMode::Human
            };
            let cfg = PipelineConfig {
                mode,
                headless: true,
                dry_run,
                skip_adapters: true,
                skip_service,
                at_login: false,
                strict_scopes: false,
                with_hooks: false,
                gmail: AdapterChoice::Skip,
                github: AdapterChoice::Skip,
                fs: vec![],
                only: None,
                db_path: cli.db.clone(),
                bind: "127.0.0.1:7777".to_string(),
                wire_bind: "127.0.0.1:7778".to_string(),
            };
            offboard(cfg, purge).await?;
        }

        Commands::Doctor { json } => {
            let checks = ctxd_cli::onboard::doctor::run(&cli.db).await;
            if json {
                let summary = ctxd_cli::onboard::doctor::Summary::from_checks(&checks);
                let body = serde_json::json!({
                    "checks": checks,
                    "summary": {
                        "total": summary.total,
                        "ok": summary.ok,
                        "warnings": summary.warnings,
                        "failed": summary.failed,
                        "skipped": summary.skipped,
                    },
                });
                println!("{}", serde_json::to_string_pretty(&body)?);
                if !summary.all_ok() {
                    std::process::exit(1);
                }
            } else {
                let all_ok = ctxd_cli::onboard::doctor::render_human(&checks);
                if !all_ok {
                    std::process::exit(1);
                }
            }
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

/// Parse one step slug from `--only` into the typed [`StepName`].
/// Unknown slugs are dropped silently — the protocol's stable
/// kebab-case slugs are the contract; any string outside that set
/// shouldn't crash the CLI.
fn parse_step(slug: &str) -> Option<ctxd_cli::onboard::protocol::StepName> {
    use ctxd_cli::onboard::protocol::StepName;
    match slug {
        "snapshot" => Some(StepName::Snapshot),
        "service-install" => Some(StepName::ServiceInstall),
        "service-start" => Some(StepName::ServiceStart),
        "configure-clients" => Some(StepName::ConfigureClients),
        "mint-capabilities" => Some(StepName::MintCapabilities),
        "seed-subjects" => Some(StepName::SeedSubjects),
        "configure-adapters" => Some(StepName::ConfigureAdapters),
        "doctor" => Some(StepName::Doctor),
        _ => None,
    }
}

/// Open `url` in the system browser. Cross-platform via the OS-native
/// `open` (macOS), `xdg-open` (Linux), `cmd /c start ""` (Windows).
/// No external dependency.
fn open_browser(url: &str) -> Result<()> {
    let mut cmd = if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    } else if cfg!(target_os = "windows") {
        // The leading "" is the title for `start`; without it `start`
        // treats the URL as a window title and does nothing.
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", "", url]);
        c
    } else {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };
    let status = cmd
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .with_context(|| format!("failed to spawn browser opener for {url}"))?;
    if !status.success() {
        return Err(anyhow::anyhow!(
            "browser opener exited with non-zero status: {status}"
        ));
    }
    Ok(())
}

/// Initialize the tracing subscriber. If `OTEL_EXPORTER_OTLP_ENDPOINT` is set,
/// an OpenTelemetry tracing layer is added that exports spans over OTLP/gRPC.
/// Otherwise only the plain fmt layer is used (zero OTEL overhead).
fn init_tracing() -> OtelGuard {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let fmt_layer = tracing_subscriber::fmt::layer().with_writer(std::io::stderr);

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
