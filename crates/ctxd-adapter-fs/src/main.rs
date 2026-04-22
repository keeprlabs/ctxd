//! CLI entry point for the filesystem adapter.
//!
//! Usage: `ctxd-adapter-fs --watch /path/to/dir --db ctxd.db`

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use ctxd_adapter_core::{Adapter, AdapterError, AppendEvent, AsyncDirectSink};
use ctxd_core::event::Event;
use ctxd_store::EventStore;
use tracing_subscriber::EnvFilter;

/// Filesystem adapter for ctxd — watches a directory and publishes file change events.
#[derive(Parser, Debug)]
#[command(name = "ctxd-adapter-fs")]
#[command(about = "Watch a directory for file changes and publish events to ctxd")]
struct Cli {
    /// Directory to watch for file changes.
    #[arg(long)]
    watch: PathBuf,

    /// Path to the ctxd SQLite database.
    #[arg(long)]
    db: PathBuf,
}

/// Wraps an EventStore to implement AppendEvent.
struct StoreAppender {
    store: EventStore,
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
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();

    let store = EventStore::open(&cli.db).await?;

    let appender = Arc::new(StoreAppender { store });
    let sink = AsyncDirectSink::new("ctxd://localhost".to_string(), appender);

    let adapter = ctxd_adapter_fs::FsAdapter::new(cli.watch);
    adapter.run(Box::new(sink)).await?;

    Ok(())
}
