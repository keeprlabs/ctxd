//! CLI entry point for `ctxd-adapter-gmail`.
//!
//! Subcommands:
//! - `auth` — runs OAuth2 device-code flow and persists an encrypted
//!   refresh token.
//! - `run` — loads the encrypted token, refreshes the access token, and
//!   syncs the inbox via the Gmail History API.
//! - `status` — prints the current sync state.
//!
//! Credentials are accepted via `--client-id` / `--client-secret` flags
//! or the `GOOGLE_CLIENT_ID` / `GOOGLE_CLIENT_SECRET` environment
//! variables. They are never written to disk and never logged.

use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};
use ctxd_adapter_core::{Adapter, AdapterError, AppendEvent, AsyncDirectSink};
use ctxd_adapter_gmail::{
    crypto, gmail, oauth, state, GmailAdapter, GmailAdapterConfig, DEFAULT_FETCH_CONCURRENCY,
    DEFAULT_LABELS, DEFAULT_POLL_INTERVAL_SECS, GMAIL_SCOPE,
};
use ctxd_core::event::Event;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

/// Default state-dir under `$XDG_STATE_HOME/ctxd-adapter-gmail` or
/// `~/.local/state/ctxd-adapter-gmail`.
fn default_state_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_STATE_HOME") {
        return PathBuf::from(xdg).join("ctxd-adapter-gmail");
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home)
            .join(".local")
            .join("state")
            .join("ctxd-adapter-gmail");
    }
    PathBuf::from(".").join("ctxd-adapter-gmail-state")
}

/// CLI definition.
#[derive(Parser, Debug)]
#[command(name = "ctxd-adapter-gmail", version)]
#[command(about = "Gmail adapter for ctxd — ingests emails via the Gmail API")]
struct Cli {
    /// Subcommand to run.
    #[command(subcommand)]
    command: Command,
}

/// Subcommands.
#[derive(Subcommand, Debug)]
enum Command {
    /// Run the OAuth2 device-code flow and persist an encrypted refresh token.
    Auth(AuthArgs),
    /// Run the sync loop: refresh the token, sync the inbox, publish events.
    Run(RunArgs),
    /// Print the current sync state.
    Status(StatusArgs),
}

/// Arguments for `auth`.
#[derive(Parser, Debug)]
struct AuthArgs {
    /// State directory for keys, tokens, and the sync DB.
    #[arg(long, default_value_os_t = default_state_dir())]
    state_dir: PathBuf,
    /// Google OAuth2 client ID. Defaults to `$GOOGLE_CLIENT_ID`.
    #[arg(long, env = "GOOGLE_CLIENT_ID")]
    client_id: String,
    /// Google OAuth2 client secret. Defaults to `$GOOGLE_CLIENT_SECRET`.
    #[arg(long, env = "GOOGLE_CLIENT_SECRET")]
    client_secret: String,
    /// Override the device-code endpoint (used by tests).
    #[arg(long, default_value = oauth::DEFAULT_DEVICE_CODE_URL)]
    device_code_url: String,
    /// Override the token endpoint (used by tests).
    #[arg(long, default_value = oauth::DEFAULT_TOKEN_URL)]
    token_url: String,
}

/// Arguments for `run`.
#[derive(Parser, Debug)]
struct RunArgs {
    /// State directory for keys, tokens, and the sync DB.
    #[arg(long, default_value_os_t = default_state_dir())]
    state_dir: PathBuf,
    /// Path to the ctxd SQLite database to publish into. If unset,
    /// connects to a remote ctxd daemon at `--ctxd-url` (not yet
    /// implemented).
    #[arg(long)]
    db: Option<PathBuf>,
    /// Remote ctxd daemon URL. Used when `--db` is unset. Currently
    /// only `tcp://...` is supported by the wire protocol; this
    /// adapter falls back to a friendly error if a remote URL is
    /// passed without `--db`.
    #[arg(long, default_value = "tcp://127.0.0.1:7778")]
    ctxd_url: String,
    /// Capability token (base64) authorizing publish on the target
    /// subject prefix. Reserved for the remote-mode path; ignored when
    /// `--db` is set.
    #[arg(long)]
    cap: Option<String>,
    /// Gmail user id.
    #[arg(long, default_value = "me")]
    user_id: String,
    /// Comma-separated label list to sync (default `INBOX,SENT`).
    #[arg(long, value_delimiter = ',')]
    labels: Option<Vec<String>>,
    /// Polling interval, e.g. `60s`, `5m`. Plain integers are
    /// interpreted as seconds.
    #[arg(long, default_value = "60s")]
    poll_interval: String,
    /// OAuth client id (refresh path).
    #[arg(long, env = "GOOGLE_CLIENT_ID")]
    client_id: String,
    /// OAuth client secret (refresh path).
    #[arg(long, env = "GOOGLE_CLIENT_SECRET")]
    client_secret: String,
    /// Override the token endpoint (used by tests).
    #[arg(long, default_value = oauth::DEFAULT_TOKEN_URL)]
    token_url: String,
    /// Override the Gmail API base URL (used by tests).
    #[arg(long, default_value = gmail::DEFAULT_API_BASE)]
    api_base: String,
    /// Run a single sync iteration and exit (used by tests).
    #[arg(long)]
    run_once: bool,
}

/// Arguments for `status`.
#[derive(Parser, Debug)]
struct StatusArgs {
    /// State directory.
    #[arg(long, default_value_os_t = default_state_dir())]
    state_dir: PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    match cli.command {
        Command::Auth(a) => cmd_auth(a).await?,
        Command::Run(r) => cmd_run(r).await?,
        Command::Status(s) => cmd_status(s).await?,
    }
    Ok(())
}

async fn cmd_auth(args: AuthArgs) -> anyhow::Result<()> {
    tokio::fs::create_dir_all(&args.state_dir).await?;
    let key_path = args.state_dir.join("gmail.key");
    let token_path = args.state_dir.join("gmail.token.enc");

    let master_key = crypto::load_or_create_master_key(&key_path).await?;

    let oauth_cfg = oauth::OAuthConfig {
        client_id: args.client_id,
        client_secret: args.client_secret,
        scope: GMAIL_SCOPE.to_string(),
        device_code_url: args.device_code_url,
        token_url: args.token_url,
    };

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;

    info!("requesting device code...");
    let code = oauth::request_device_code(&http, &oauth_cfg).await?;

    let mut stderr = io::stderr().lock();
    writeln!(stderr, "==============================================")?;
    writeln!(stderr, "Open this URL in a browser:")?;
    writeln!(stderr, "  {}", code.verification_url)?;
    writeln!(stderr, "Enter this code:")?;
    writeln!(stderr, "  {}", code.user_code)?;
    writeln!(stderr, "==============================================")?;
    writeln!(stderr, "(waiting for authorization...)")?;
    drop(stderr);

    let tokens = oauth::poll_for_tokens(&http, &oauth_cfg, &code).await?;
    info!("authorization complete; persisting refresh token");

    let blob = crypto::encrypt(&master_key, tokens.refresh_token.as_bytes())?;
    crypto::write_secret_file(&token_path, &blob).await?;

    info!(state_dir = %args.state_dir.display(), "tokens written");
    Ok(())
}

async fn cmd_run(args: RunArgs) -> anyhow::Result<()> {
    let labels: Vec<String> = args
        .labels
        .clone()
        .unwrap_or_else(|| DEFAULT_LABELS.iter().map(|s| (*s).to_string()).collect());

    let poll_interval = parse_duration(&args.poll_interval)
        .unwrap_or_else(|| Duration::from_secs(DEFAULT_POLL_INTERVAL_SECS));

    let oauth_cfg = oauth::OAuthConfig {
        client_id: args.client_id,
        client_secret: args.client_secret,
        scope: GMAIL_SCOPE.to_string(),
        device_code_url: oauth::DEFAULT_DEVICE_CODE_URL.to_string(),
        token_url: args.token_url,
    };

    let gmail_cfg = gmail::GmailClientConfig {
        api_base: args.api_base,
        user_id: args.user_id.clone(),
        ..Default::default()
    };

    let cfg = GmailAdapterConfig {
        state_dir: args.state_dir.clone(),
        user_id: args.user_id,
        labels,
        poll_interval,
        oauth: oauth_cfg,
        gmail: gmail_cfg,
        run_once: args.run_once,
        token_path_override: None,
        key_path_override: None,
        db_path_override: None,
    };

    let adapter = GmailAdapter::new(cfg);

    let db_path = args.db.ok_or_else(|| {
        anyhow::anyhow!(
            "remote-mode (--ctxd-url) is reserved for the federation wire; \
             v0.3 requires --db to point at a local ctxd SQLite DB. \
             cap={:?}, ctxd_url={}",
            args.cap.is_some(),
            args.ctxd_url
        )
    })?;
    let store = ctxd_store::EventStore::open(&db_path).await?;
    let appender: Arc<dyn AppendEvent> = Arc::new(StoreAppender { store });
    let sink = AsyncDirectSink::new("ctxd://gmail".to_string(), appender);

    if let Err(e) = adapter.run(Box::new(sink)).await {
        warn!(err = %e, "gmail adapter exited with error");
        return Err(anyhow::anyhow!(e));
    }
    Ok(())
}

async fn cmd_status(args: StatusArgs) -> anyhow::Result<()> {
    let cfg = GmailAdapterConfig {
        state_dir: args.state_dir.clone(),
        user_id: "me".to_string(),
        labels: vec![],
        poll_interval: Duration::from_secs(DEFAULT_POLL_INTERVAL_SECS),
        oauth: oauth::OAuthConfig::google(String::new(), String::new(), GMAIL_SCOPE.to_string()),
        gmail: gmail::GmailClientConfig::default(),
        run_once: true,
        token_path_override: None,
        key_path_override: None,
        db_path_override: None,
    };

    let token_path = cfg.token_path();
    let key_path = cfg.key_path();
    let db_path = cfg.db_path();

    println!("ctxd-adapter-gmail status");
    println!("  state_dir   : {}", args.state_dir.display());
    println!("  master_key  : {}", key_path.display());
    println!("  token_file  : {}", token_path.display());
    println!("  state_db    : {}", db_path.display());
    println!(
        "  token_present: {}",
        tokio::fs::try_exists(&token_path).await.unwrap_or(false)
    );

    if tokio::fs::try_exists(&db_path).await.unwrap_or(false) {
        let store = state::StateStore::open(&db_path).await?;
        let cursor = store.cursor().await?;
        println!(
            "  last_history_id: {}",
            cursor.history_id.as_deref().unwrap_or("(none)")
        );
        println!(
            "  last_poll_at  : {}",
            cursor
                .last_poll_at
                .map(|t| t.to_rfc3339())
                .unwrap_or_else(|| "(never)".to_string())
        );
        println!("  published_count: {}", store.published_count().await?);
    } else {
        println!("  (state DB not yet created)");
    }
    println!("  fetch_concurrency: {DEFAULT_FETCH_CONCURRENCY}");
    Ok(())
}

/// Parse a duration like `60s`, `5m`, `1h`, or a bare integer (seconds).
fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    if let Some(num) = s.strip_suffix("ms") {
        return num.parse::<u64>().ok().map(Duration::from_millis);
    }
    if let Some(num) = s.strip_suffix('s') {
        return num.parse::<u64>().ok().map(Duration::from_secs);
    }
    if let Some(num) = s.strip_suffix('m') {
        return num.parse::<u64>().ok().map(|n| Duration::from_secs(n * 60));
    }
    if let Some(num) = s.strip_suffix('h') {
        return num
            .parse::<u64>()
            .ok()
            .map(|n| Duration::from_secs(n * 3600));
    }
    s.parse::<u64>().ok().map(Duration::from_secs)
}

/// Wraps `EventStore` so it can be used as an `AppendEvent`.
struct StoreAppender {
    store: ctxd_store::EventStore,
}

#[async_trait::async_trait]
impl AppendEvent for StoreAppender {
    async fn append(&self, event: Event) -> Result<String, AdapterError> {
        let stored = self
            .store
            .append(event)
            .await
            .map_err(|e| AdapterError::Internal(format!("store error: {e}")))?;
        Ok(stored.id.to_string())
    }
}
