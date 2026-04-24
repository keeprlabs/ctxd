//! CLI entry point for the GitHub adapter.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};
use ctxd_adapter_core::{Adapter, AdapterError, AppendEvent, AsyncDirectSink, EventSink};
use ctxd_adapter_github::state::StateDb;
use ctxd_adapter_github::{
    config::{Config, RepoRef, RepoSelector, ResourceKind},
    GitHubAdapter,
};
use ctxd_core::event::Event;
use tracing_subscriber::EnvFilter;

/// GitHub adapter for ctxd.
#[derive(Parser, Debug)]
#[command(name = "ctxd-adapter-github", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run the adapter, polling GitHub on an interval.
    Run(RunArgs),
    /// Print current cursors, last poll, rate-limit info.
    Status(StatusArgs),
}

#[derive(Parser, Debug, Clone)]
struct RunArgs {
    /// ctxd daemon URL (TCP wire protocol). Currently informational; the
    /// adapter writes via a local SQLite store specified by `--db`. A future
    /// release will use this to attach over the network.
    #[arg(long, default_value = "tcp://127.0.0.1:7778")]
    ctxd_url: String,

    /// ctxd capability (base64). Currently unused — kept for forward
    /// compatibility with the wire-protocol auth handshake.
    #[arg(long)]
    cap: Option<String>,

    /// Path to the ctxd SQLite event store.
    #[arg(long)]
    db: PathBuf,

    /// GitHub PAT. If omitted, falls back to `$GITHUB_TOKEN`.
    #[arg(long, env = "GITHUB_TOKEN", hide_env_values = true)]
    token: String,

    /// Repos to poll, in `owner/name` form. Repeatable. Mutually exclusive
    /// with `--user`.
    #[arg(long, value_parser = parse_repo)]
    repo: Vec<RepoRef>,

    /// Poll every repo accessible to the authenticated user.
    #[arg(long, conflicts_with = "repo")]
    user: bool,

    /// Where the state DB lives. Defaults to
    /// `$XDG_STATE_HOME/ctxd-adapter-github` (or
    /// `~/.local/state/ctxd-adapter-github` on Linux,
    /// `~/Library/Application Support/ctxd-adapter-github` on macOS).
    #[arg(long)]
    state_dir: Option<PathBuf>,

    /// Polling interval (e.g., `60s`, `5m`).
    #[arg(long, default_value = "60s", value_parser = parse_duration)]
    poll_interval: Duration,

    /// Include `/notifications` polling for the authenticated user.
    #[arg(long, default_value_t = true)]
    include_notifications: bool,

    /// Comma-separated list of kinds to poll: `issues,pulls,comments,notifications`.
    #[arg(
        long,
        default_value = "issues,pulls,comments,notifications",
        value_delimiter = ',',
        value_parser = parse_kind
    )]
    kinds: Vec<ResourceKind>,

    /// Override the GitHub API base URL (defaults to https://api.github.com).
    /// Used by tests to point at a mock server.
    #[arg(long, default_value = "https://api.github.com", hide = true)]
    api_base: String,
}

#[derive(Parser, Debug, Clone)]
struct StatusArgs {
    /// State directory to inspect (same default as `run --state-dir`).
    #[arg(long)]
    state_dir: Option<PathBuf>,
}

fn parse_repo(s: &str) -> Result<RepoRef, String> {
    RepoRef::parse(s)
}

fn parse_kind(s: &str) -> Result<ResourceKind, String> {
    ResourceKind::parse(s)
}

/// Parse a Go-ish duration string: `60s`, `5m`, `1h`, plain `123` is seconds.
fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    if let Ok(secs) = s.parse::<u64>() {
        return Ok(Duration::from_secs(secs));
    }
    let (num, unit) = s.split_at(s.len().saturating_sub(1));
    let n: u64 = num
        .parse()
        .map_err(|e| format!("invalid duration {s}: {e}"))?;
    let secs = match unit {
        "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        other => return Err(format!("unknown duration unit: {other}")),
    };
    Ok(Duration::from_secs(secs))
}

fn default_state_dir() -> PathBuf {
    if let Some(d) = dirs::state_dir() {
        return d.join("ctxd-adapter-github");
    }
    if let Some(d) = dirs::data_local_dir() {
        return d.join("ctxd-adapter-github");
    }
    PathBuf::from(".ctxd-adapter-github")
}

/// Wraps an [`ctxd_store::EventStore`] in [`AppendEvent`].
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Cmd::Run(args) => run_cmd(args).await,
        Cmd::Status(args) => status_cmd(args).await,
    }
}

async fn run_cmd(args: RunArgs) -> anyhow::Result<()> {
    if args.token.is_empty() {
        anyhow::bail!("missing token: pass --token or set GITHUB_TOKEN");
    }
    if !args.user && args.repo.is_empty() {
        anyhow::bail!("specify at least one --repo owner/name, or pass --user");
    }
    let repos = if args.user {
        RepoSelector::AuthenticatedUser
    } else {
        RepoSelector::Explicit(args.repo.clone())
    };
    let state_dir = args.state_dir.clone().unwrap_or_else(default_state_dir);

    let cfg = Config {
        api_base: args.api_base.clone(),
        token: args.token.clone(),
        repos,
        state_dir,
        poll_interval: args.poll_interval,
        kinds: args.kinds.clone(),
        include_notifications: args.include_notifications,
        max_cycles: None,
    };

    let store = ctxd_store::EventStore::open(&args.db).await?;
    let appender = Arc::new(StoreAppender { store });
    let sink: Box<dyn EventSink> = Box::new(AsyncDirectSink::new(
        format!("ctxd-adapter-github://{}", args.ctxd_url),
        appender,
    ));

    let adapter = GitHubAdapter::new(cfg);
    adapter.run(sink).await?;
    Ok(())
}

async fn status_cmd(args: StatusArgs) -> anyhow::Result<()> {
    let state_dir = args.state_dir.clone().unwrap_or_else(default_state_dir);
    let state = StateDb::open(&state_dir).await?;
    let cursors = state.list_cursors().await?;
    let last = state.get_meta("last_poll_at").await?;
    let rate_remaining = state.get_meta("rate_remaining").await?;
    let rate_reset = state.get_meta("rate_reset").await?;

    println!("state_dir       : {}", state_dir.display());
    println!("last_poll_at    : {}", last.as_deref().unwrap_or("(never)"));
    println!(
        "rate_remaining  : {}",
        rate_remaining.as_deref().unwrap_or("(unknown)")
    );
    println!(
        "rate_reset      : {}",
        rate_reset.as_deref().unwrap_or("(unknown)")
    );
    if cursors.is_empty() {
        println!("cursors         : (none)");
    } else {
        println!("cursors:");
        for (scope, kind, since) in cursors {
            println!("  {scope:30} {kind:18} since={since}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_parses_seconds() {
        assert_eq!(parse_duration("60s").unwrap(), Duration::from_secs(60));
        assert_eq!(parse_duration("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
        assert_eq!(parse_duration("42").unwrap(), Duration::from_secs(42));
        assert!(parse_duration("garbage").is_err());
    }

    #[test]
    fn cli_parses_run_with_required_args() {
        let cli = Cli::try_parse_from([
            "ctxd-adapter-github",
            "run",
            "--db",
            "/tmp/x.db",
            "--token",
            "tk",
            "--repo",
            "acme/web",
        ])
        .expect("parses");
        match cli.command {
            Cmd::Run(args) => {
                assert_eq!(args.repo.len(), 1);
                assert_eq!(args.token, "tk");
            }
            _ => panic!("wrong command"),
        }
    }

    #[test]
    fn cli_help_clean() {
        // try_parse_from with --help returns an Err that prints the help.
        let err = Cli::try_parse_from(["ctxd-adapter-github", "--help"]).unwrap_err();
        let s = err.to_string();
        assert!(s.contains("ctxd-adapter-github"));
        assert!(s.contains("run"));
        assert!(s.contains("status"));
    }

    #[test]
    fn cli_run_help_lists_flags() {
        let err = Cli::try_parse_from(["ctxd-adapter-github", "run", "--help"]).unwrap_err();
        let s = err.to_string();
        assert!(s.contains("--db"));
        assert!(s.contains("--token"));
        assert!(s.contains("--repo"));
        assert!(s.contains("--user"));
        assert!(s.contains("--state-dir"));
        assert!(s.contains("--poll-interval"));
        assert!(s.contains("--kinds"));
    }
}
