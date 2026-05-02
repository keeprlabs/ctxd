//! ctxd daemon serve loop, factored out of `main.rs` so other CLI
//! subcommands (notably `ctxd dashboard`, landing in v0.4) can reuse
//! the same bootstrap path without duplicating ~150 lines of setup.
//!
//! The legacy `ctxd serve` calls [`serve`] with a [`ServeConfig`] that
//! enables every transport (HTTP admin, wire protocol, MCP, federation).
//! Other entry points pass a tighter config: e.g. `ctxd dashboard` will
//! set `wire_bind: None`, `mcp_stdio: false`, and `federation: false` so
//! only the HTTP admin (with the embedded dashboard) starts.

use anyhow::{Context, Result};
use ctxd_cap::state::{CaveatState, PendingApproval};
use ctxd_cap::CapEngine;
use ctxd_core::event::Event;
use ctxd_core::subject::Subject;
use ctxd_http::router::{allowed_hosts_for_bind, build_router_with_hosts};
use ctxd_mcp::CtxdMcpServer;
use ctxd_store::EventStore;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use crate::pidfile::{self, DaemonState, PidFile, PidfileGuard};
use crate::ready;

/// Configuration for the daemon serve loop.
///
/// Field defaults mirror the historical `ctxd serve` behavior; new
/// entry points (dashboard) override the fields they don't want.
#[derive(Debug, Clone)]
pub struct ServeConfig {
    /// Address to bind the HTTP admin API (e.g. `"127.0.0.1:7777"`).
    pub bind: String,

    /// Address to bind the wire protocol (MessagePack over TCP).
    /// `None` skips the wire server entirely — used by `ctxd dashboard`.
    pub wire_bind: Option<String>,

    /// Run MCP server on stdio (for Claude Desktop / mcp-inspector).
    pub mcp_stdio: bool,

    /// Bind a legacy MCP SSE transport at this address. `None` disables.
    pub mcp_sse: Option<String>,

    /// Bind a streamable-HTTP MCP transport at this address. `None` disables.
    pub mcp_http: Option<String>,

    /// Require every `tools/call` on the HTTP transports to carry a
    /// capability token.
    pub require_auth: bool,

    /// Embedder backend: `null`, `openai`, or `ollama`.
    pub embedder: String,

    /// Override the embedder model.
    pub embedder_model: Option<String>,

    /// Override the embedder base URL.
    pub embedder_url: Option<String>,

    /// API key for the embedder (OpenAI only).
    pub embedder_api_key: Option<String>,

    /// Storage backend kind: `sqlite` (default), `postgres`, or
    /// `duckdb-object`.
    pub storage: String,

    /// URI for non-sqlite backends.
    pub storage_uri: Option<String>,

    /// Bootstrap federation (signing key, peer manager, replication
    /// tasks). `false` skips federation entirely — used by `ctxd
    /// dashboard`.
    pub federation: bool,

    /// Path to the SQLite DB the daemon is opened against. `Some`
    /// enables the pidfile lock (written next to the DB at
    /// `<db_path>.pid`) and the cross-platform "ready" signal. `None`
    /// disables both, which is appropriate for in-memory test
    /// fixtures and the `--storage` non-default backends that don't
    /// have a single canonical local path.
    pub db_path: Option<PathBuf>,
}

/// Run the ctxd daemon with the given configuration.
///
/// `store`, `cap_engine`, `caveat_state`, and `pending_approval_tx` are
/// constructed by `main` (shared across subcommands) and passed in
/// rather than rebuilt here, keeping this a pure refactor of the prior
/// `Commands::Serve` arm.
pub async fn serve(
    cfg: ServeConfig,
    mut store: EventStore,
    cap_engine: Arc<CapEngine>,
    caveat_state: Arc<dyn CaveatState>,
    pending_approval_tx: tokio::sync::broadcast::Sender<PendingApproval>,
) -> Result<()> {
    // Backend selection. The default sqlite path runs the
    // full daemon (HTTP admin + wire + MCP + federation)
    // because the legacy concrete-typed call sites still
    // require `EventStore`. Non-sqlite backends are
    // routed through the trait-based `select_store` path
    // and serve a minimal HTTP admin only — federation /
    // MCP / wire-protocol over `dyn Store` is queued for
    // v0.4 (see ADR 019).
    use crate::storage_selector::{select_store, StorageKind, StorageSpec};
    let kind = StorageKind::parse(&cfg.storage).map_err(|e| anyhow::anyhow!(e))?;
    if kind != StorageKind::Sqlite {
        let spec = StorageSpec {
            kind,
            sqlite_path: None,
            uri: cfg.storage_uri.clone(),
        };
        let dyn_store = select_store(&spec)
            .await
            .map_err(|e| anyhow::anyhow!("select_store: {e}"))?;
        tracing::warn!(
            backend = ?kind,
            "non-sqlite --storage selected; running minimal HTTP admin only \
             (wire/federation/MCP over dyn Store is v0.4 — see ADR 019)"
        );
        return run_minimal_serve(cfg.bind, dyn_store).await;
    }
    let _ = cfg.storage_uri; // unused for sqlite default path
    let addr: SocketAddr = cfg.bind.parse().context("invalid bind address")?;
    tracing::info!("starting ctxd daemon on {addr}");

    // Pre-flight: if the pidfile alongside our DB names a live
    // daemon that's still answering /health, refuse to start. This
    // converts the EADDRINUSE-deep-in-bind footgun (which the user
    // sees as a stack of context-wrapped IO errors) into a friendly
    // single-line refusal at the top of the function.
    //
    // Stale and unresponsive pidfiles are tolerated — we log and
    // continue. The bind() call below is the actual mutual-exclusion
    // boundary, and a real port collision still surfaces as EADDRINUSE
    // with the existing friendly hint in main.rs's `Dashboard` arm.
    if let Some(db_path) = &cfg.db_path {
        match pidfile::detect(db_path).await {
            DaemonState::Running(pf) => {
                anyhow::bail!(
                    "ctxd is already running:\n  \
                     pid:        {pid}\n  \
                     admin URL:  http://{admin}\n  \
                     started:    {started}\n  \
                     version:    {version}\n\n\
                     Stop it first (`kill {pid}` or `ctxd offboard --service-only`), \
                     or pass --bind 127.0.0.1:0 with a separate --db to start an \
                     additional daemon on a different port.",
                    pid = pf.pid,
                    admin = pf.admin_bind,
                    started = pf.started_at.to_rfc3339(),
                    version = pf.version,
                );
            }
            DaemonState::Unresponsive(pf) => {
                tracing::warn!(
                    pid = pf.pid,
                    admin = %pf.admin_bind,
                    "another ctxd process holds the pidfile but its /health is \
                     unresponsive; continuing — bind may fail with EADDRINUSE if it \
                     actually still owns the port"
                );
            }
            DaemonState::Stale(pf) => {
                tracing::info!(
                    pid = pf.pid,
                    "stale pidfile from prior daemon (pid is dead); will overwrite"
                );
            }
            DaemonState::NotRunning => {}
        }
    }
    // Keep the broadcast sender alive for the lifetime of `serve`
    // so future adapters can `.subscribe()` at any point.
    let _approval_tx = pending_approval_tx.clone();

    // Construct the embedder up front so we fail loudly on
    // misconfiguration rather than at first auto-embed.
    let choice =
        crate::embedder::EmbedderChoice::parse(&cfg.embedder).map_err(|e| anyhow::anyhow!(e))?;
    let embed_opts = crate::embedder::EmbedderOpts {
        model: cfg.embedder_model,
        url: cfg.embedder_url,
        api_key: cfg.embedder_api_key,
        dimensions: None,
    };
    let embedder_arc = crate::embedder::build_embedder(choice, embed_opts)
        .context("failed to construct embedder")?;
    tracing::info!(
        kind = %embedder_arc.kind(),
        model = embedder_arc.model(),
        dimensions = embedder_arc.dimensions(),
        "embedder ready"
    );
    // Install on the store so `append` auto-embeds when the
    // event has indexable text.
    store.set_embedder(embedder_arc.clone());
    // Open the persisted HNSW index. The dimensions match
    // the active embedder so a previously-persisted index
    // built with a different model is detected as a
    // dimension mismatch and rebuilt.
    let vec_cfg = ctxd_store::views::vector::VectorIndexConfig {
        dimensions: embedder_arc.dimensions(),
        ..Default::default()
    };
    let _vec_idx = store
        .ensure_vector_index(vec_cfg)
        .await
        .context("failed to open HNSW vector index")?;

    // Daemon path: compose the JSON API, the dashboard frontend, and
    // the loopback-or-cap-token middleware that fronts both.
    //
    // Composition order (outermost layer wins):
    //   1. host_check + defensive_headers (already inside build_router_with_hosts)
    //   2. localhost_or_cap_token (added below)
    // Loopback callers (the dashboard's browser) bypass cap-token; remote
    // callers with a valid admin token still pass; everyone else gets 403.
    //
    // The bind site MUST use into_make_service_with_connect_info so the
    // ConnectInfo<SocketAddr> extension is populated — otherwise the
    // loopback middleware fails closed with a 500.
    let api = build_router_with_hosts(
        store.clone(),
        cap_engine.clone(),
        caveat_state.clone(),
        allowed_hosts_for_bind(addr),
    );
    let frontend = ctxd_dashboard::router::<()>();
    let app = ctxd_dashboard::apply_localhost_or_cap_token(api.merge(frontend), cap_engine.clone());
    // Bind synchronously so EADDRINUSE is returned as an error from
    // serve() rather than killing a spawned worker task. Callers
    // (including `ctxd dashboard`) rely on this to print friendly
    // port-already-in-use guidance.
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind HTTP admin to {addr}"))?;
    let bound_addr = listener.local_addr().unwrap_or(addr);
    tracing::info!("HTTP admin API + dashboard listening on {bound_addr}");

    // Pidfile is written **after** bind succeeds (so we don't lie if
    // bind fails) but **before** we spawn the serve task (so any
    // observer who saw the marker line below can rely on the
    // pidfile being present). The guard removes the file on Drop —
    // including on the panic / cancellation paths — so long as it
    // still names this PID. `None` means we're a transient (in-memory
    // tests, --storage non-default) and skip the pidfile entirely.
    let _pidfile_guard = if let Some(db_path) = cfg.db_path.as_deref() {
        let pf = PidFile {
            pid: std::process::id(),
            admin_bind: bound_addr.to_string(),
            wire_bind: cfg.wire_bind.clone(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            started_at: chrono::Utc::now(),
            db_path: db_path.to_string_lossy().into_owned(),
        };
        Some(PidfileGuard::install(db_path, &pf).context("install pidfile")?)
    } else {
        None
    };

    // Notify launchd / systemd / external pollers that the daemon
    // has finished startup. On Linux this fires `READY=1` so a
    // `Type=notify` unit transitions out of activating; on macOS we
    // emit a parseable stderr marker line that launchd's
    // StandardErrorPath can be tailed for.
    let admin_url = format!("http://{bound_addr}");
    let wire_url_owned = cfg.wire_bind.as_deref().map(|w| format!("tcp://{w}"));
    ready::signal_ready(&admin_url, wire_url_owned.as_deref());

    let http_handle = tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await;
    });

    // Federation bootstrap (signing key + peer manager + replication
    // tasks). Skipped when `cfg.federation` is false (e.g. `ctxd
    // dashboard` mode).
    let (wire_handle, _replication_handle) = if cfg.federation {
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

        // Wire protocol server (MessagePack over TCP) is paired with
        // federation today: PeerManager needs the wire's event sender.
        // If `wire_bind` is None but `federation` is true, that's a
        // configuration error — federation requires the wire transport.
        let wire_bind = cfg.wire_bind.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "federation=true requires wire_bind to be set; \
                 federation cannot be bootstrapped without the wire \
                 transport"
            )
        })?;
        let wire_addr: SocketAddr = wire_bind.parse().context("invalid wire bind address")?;

        let wire_server =
            crate::protocol::ProtocolServer::new(store.clone(), cap_engine.clone(), wire_addr);
        let event_tx_for_fed = wire_server.event_sender();
        let fed = std::sync::Arc::new(crate::federation::PeerManager::new(
            std::sync::Arc::new(store.clone()),
            cap_engine.clone(),
            local_peer_id,
            signing_bytes,
            event_tx_for_fed,
            crate::federation::AutoAcceptPolicy::from_env(),
        ));
        // Re-enroll persisted peers so outbound replication can
        // dial them without manual `peer add` after restart.
        for p in store.peer_list_impl().await? {
            fed.enroll(crate::federation::EnrolledPeer {
                peer_id: p.peer_id.clone(),
                remote_pubkey: p.public_key,
                remote_grants_us: p.granted_subjects.clone(),
                we_grant_remote: p.granted_subjects,
                cap_from_remote: None,
                cap_for_remote: None,
            })
            .await;
        }
        let replication_handle = fed.start_replication_tasks();
        let wire_server = wire_server.with_federation(fed.clone());
        let wire_handle = tokio::spawn(async move {
            if let Err(e) = wire_server.run().await {
                tracing::error!("Wire protocol server error: {e}");
            }
        });
        (Some(wire_handle), Some(replication_handle))
    } else if let Some(wire_bind) = cfg.wire_bind.as_deref() {
        // Wire protocol without federation: useful for testing /
        // single-node deployments. Same bind, no PeerManager attached.
        let wire_addr: SocketAddr = wire_bind.parse().context("invalid wire bind address")?;
        let wire_server =
            crate::protocol::ProtocolServer::new(store.clone(), cap_engine.clone(), wire_addr);
        let wire_handle = tokio::spawn(async move {
            if let Err(e) = wire_server.run().await {
                tracing::error!("Wire protocol server error: {e}");
            }
        });
        (Some(wire_handle), None)
    } else {
        (None, None)
    };

    // Build the shared MCP server. Each transport gets its own
    // logical clone (CtxdMcpServer is cheap to clone — store +
    // cap engine + caveat state are Arc-backed). The embedder
    // is attached so ctx_search can do vector + hybrid modes.
    let mcp_server = CtxdMcpServer::new(
        store.clone(),
        cap_engine.clone(),
        caveat_state.clone(),
        format!("ctxd://{addr}"),
    )
    .with_embedder(embedder_arc.clone());

    // Auth policy applies to HTTP transports only. Stdio is
    // local-subprocess and keeps the legacy "open by default"
    // behaviour for backwards compatibility.
    let policy = if cfg.require_auth {
        ctxd_mcp::auth::AuthPolicy::Required
    } else {
        ctxd_mcp::auth::AuthPolicy::Optional
    };

    // Shared cancellation token so SIGTERM / Ctrl-C tears every
    // transport down. The CLI doesn't (yet) wire signal handling
    // explicitly — the parent's cancellation propagates via
    // tokio's main exit.
    let shutdown = tokio_util::sync::CancellationToken::new();

    let mut sse_handle: Option<tokio::task::JoinHandle<()>> = None;
    if let Some(sse_bind) = cfg.mcp_sse {
        let sse_addr: SocketAddr = sse_bind.parse().context("invalid --mcp-sse address")?;
        let server_clone = mcp_server.clone();
        let shutdown_clone = shutdown.clone();
        sse_handle = Some(tokio::spawn(async move {
            if let Err(e) =
                ctxd_mcp::transport::run_sse(server_clone, sse_addr, policy, shutdown_clone).await
            {
                tracing::error!(error = %e, "SSE transport ended");
            }
        }));
    }

    let mut http_mcp_handle: Option<tokio::task::JoinHandle<()>> = None;
    if let Some(http_bind) = cfg.mcp_http {
        let http_addr: SocketAddr = http_bind.parse().context("invalid --mcp-http address")?;
        let server_clone = mcp_server.clone();
        let shutdown_clone = shutdown.clone();
        http_mcp_handle = Some(tokio::spawn(async move {
            if let Err(e) = ctxd_mcp::transport::run_streamable_http(
                server_clone,
                http_addr,
                policy,
                shutdown_clone,
            )
            .await
            {
                tracing::error!(error = %e, "streamable-HTTP transport ended");
            }
        }));
    }

    // Stdio is special: when it disconnects, only its task ends.
    // Sibling transports keep running. We spawn it on a task so
    // we can join all transports symmetrically.
    let stdio_handle = if cfg.mcp_stdio {
        let server_clone = mcp_server.clone();
        Some(tokio::spawn(async move {
            tracing::info!("MCP server on stdio ready");
            if let Err(e) = ctxd_mcp::transport::run_stdio(server_clone).await {
                tracing::error!(error = %e, "stdio transport ended");
            }
        }))
    } else {
        None
    };

    // Phase 3B: spawn in-process adapters declared in skills.toml
    // (under <config_dir>/ctxd/skills.toml). Each enabled adapter
    // becomes a tokio task using a daemon-owned StoreSink. Failures
    // are logged but do not crash the daemon.
    let adapter_handles = match crate::onboard::paths::config_dir() {
        Ok(cfg_dir) => {
            let manifest_path = cfg_dir.join("skills.toml");
            match crate::onboard::adapter_runtime::spawn_enabled(&store, &manifest_path) {
                Ok(handles) => {
                    if !handles.is_empty() {
                        tracing::info!(
                            count = handles.len(),
                            manifest = %manifest_path.to_string_lossy(),
                            "spawned in-process adapters"
                        );
                    }
                    handles
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to spawn adapters from skills.toml; continuing without");
                    Vec::new()
                }
            }
        }
        Err(_) => Vec::new(),
    };

    // Wait on whichever transports we started. The HTTP admin
    // API is always running; we always join its handle. The
    // wire protocol may be present depending on cfg. MCP
    // transports are optional — join when present.
    let mut handles: Vec<tokio::task::JoinHandle<()>> = vec![http_handle];
    handles.extend(adapter_handles);
    if let Some(h) = wire_handle {
        handles.push(h);
    }
    if let Some(h) = stdio_handle {
        handles.push(h);
    }
    if let Some(h) = sse_handle {
        handles.push(h);
    }
    if let Some(h) = http_mcp_handle {
        handles.push(h);
    }
    for h in handles {
        let _ = h.await;
    }
    Ok(())
}

/// Minimal HTTP admin loop for non-default storage backends. Used when
/// `--storage` is not `sqlite`: only `Store` trait methods are exposed
/// (no wire / federation / MCP) until v0.4 lifts those onto `dyn
/// Store`.
async fn run_minimal_serve(
    bind: String,
    store: std::sync::Arc<dyn ctxd_store_core::Store>,
) -> Result<()> {
    use axum::extract::{Query as AxumQuery, State as AxumState};
    use axum::routing::{get, post};
    use axum::Json;
    use std::collections::HashMap;
    let addr: SocketAddr = bind.parse().context("invalid bind address")?;
    let app_state = store.clone();
    let router = axum::Router::new()
        .route(
            "/health",
            get(|| async { Json(serde_json::json!({"ok": true, "mode": "minimal"})) }),
        )
        .route(
            "/v1/append",
            post(
                |AxumState(s): AxumState<std::sync::Arc<dyn ctxd_store_core::Store>>,
                 Json(body): Json<serde_json::Value>| async move {
                    let subject = body
                        .get("subject")
                        .and_then(|v| v.as_str())
                        .unwrap_or("/")
                        .to_string();
                    let event_type = body
                        .get("type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("event")
                        .to_string();
                    let data = body.get("data").cloned().unwrap_or(serde_json::json!({}));
                    let subject = match Subject::new(&subject) {
                        Ok(s) => s,
                        Err(e) => {
                            return (
                                axum::http::StatusCode::BAD_REQUEST,
                                Json(serde_json::json!({"error": e.to_string()})),
                            );
                        }
                    };
                    let event = Event::new("ctxd://minimal".to_string(), subject, event_type, data);
                    match s.append(event).await {
                        Ok(stored) => (
                            axum::http::StatusCode::OK,
                            Json(serde_json::json!({"id": stored.id.to_string()})),
                        ),
                        Err(e) => (
                            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                            Json(serde_json::json!({"error": e.to_string()})),
                        ),
                    }
                },
            ),
        )
        .route(
            "/v1/read",
            get(
                |AxumState(s): AxumState<std::sync::Arc<dyn ctxd_store_core::Store>>,
                 AxumQuery(q): AxumQuery<HashMap<String, String>>| async move {
                    let subject = q.get("subject").cloned().unwrap_or_else(|| "/".to_string());
                    let recursive = q
                        .get("recursive")
                        .map(|v| v == "true" || v == "1")
                        .unwrap_or(false);
                    let subject = match Subject::new(&subject) {
                        Ok(s) => s,
                        Err(e) => {
                            return (
                                axum::http::StatusCode::BAD_REQUEST,
                                Json(serde_json::json!({"error": e.to_string()})),
                            );
                        }
                    };
                    match s.read(&subject, recursive).await {
                        Ok(events) => (axum::http::StatusCode::OK, Json(serde_json::json!(events))),
                        Err(e) => (
                            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                            Json(serde_json::json!({"error": e.to_string()})),
                        ),
                    }
                },
            ),
        )
        .with_state(app_state);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .context("bind minimal admin")?;
    tracing::info!("minimal HTTP admin listening on {addr}");
    // Even in the minimal-serve path we go through
    // into_make_service_with_connect_info so the ConnectInfo
    // extension is consistent across both serve paths. The minimal
    // router doesn't currently use ConnectInfo, but downstream
    // middleware (e.g. when the dashboard composes onto this path
    // post-v0.4) will expect it.
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .context("minimal serve")?;
    Ok(())
}
