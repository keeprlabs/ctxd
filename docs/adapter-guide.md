# Adapter Guide

This guide explains how to build ingestion adapters for ctxd. An adapter
watches an external data source and publishes events into the ctxd event log.

## 1. What an Adapter Does

An adapter is a long-running process (or task) that:

1. Connects to or watches an external source — a filesystem directory, an
   email inbox, a Git repository, an API, etc.
2. Detects changes in the source.
3. Transforms each change into a ctxd event (CloudEvents v1.0 format).
4. Publishes the event via an `EventSink`.

Adapters are decoupled from the daemon. They can run in-process (sharing the
`EventStore` directly) or out-of-process (publishing over the wire protocol or
HTTP API).

## 2. The Adapter Interface

The `ctxd-adapter-core` crate defines two traits:

### `Adapter`

```rust
#[async_trait::async_trait]
pub trait Adapter: Send + Sync {
    /// Human-readable name of this adapter.
    fn name(&self) -> &str;

    /// Subject prefix this adapter writes under (e.g., "/work/local").
    fn subject_prefix(&self) -> &str;

    /// Run the adapter, publishing events via the provided sink.
    /// Should run until cancelled or until an unrecoverable error occurs.
    async fn run(&self, sink: Box<dyn EventSink>) -> Result<(), AdapterError>;
}
```

The `run` method is the main loop. It should handle transient errors gracefully
(log and continue) and only return on cancellation or fatal errors.

### `EventSink`

```rust
#[async_trait::async_trait]
pub trait EventSink: Send + Sync {
    /// Publish an event. Returns the event ID on success.
    async fn publish(
        &self,
        subject: &str,
        event_type: &str,
        data: serde_json::Value,
    ) -> Result<String, AdapterError>;
}
```

The sink abstracts away *where* events go. Two implementations are provided:

- **`DirectSink`** — writes events directly to an in-process `EventStore` via
  a callback. Zero network overhead.
- **`AsyncDirectSink`** — same idea, but with an async `AppendEvent` trait
  object for use with the real `EventStore`.

## 3. Building a Simple Adapter Step-by-Step

Let's build a minimal adapter that watches a directory for new `.txt` files.

### Step 1: Create the crate

```bash
cargo new --lib crates/ctxd-adapter-example
```

Add dependencies to `Cargo.toml`:

```toml
[dependencies]
ctxd-adapter-core = { path = "../ctxd-adapter-core" }
ctxd-core = { path = "../ctxd-core" }
serde = { workspace = true }
serde_json = { workspace = true }
tokio = { workspace = true }
tracing = { workspace = true }
async-trait = "0.1"
```

### Step 2: Implement the Adapter trait

```rust
use ctxd_adapter_core::{Adapter, AdapterError, EventSink};
use std::path::PathBuf;

pub struct ExampleAdapter {
    watch_dir: PathBuf,
}

impl ExampleAdapter {
    pub fn new(watch_dir: PathBuf) -> Self {
        Self { watch_dir }
    }
}

#[async_trait::async_trait]
impl Adapter for ExampleAdapter {
    fn name(&self) -> &str {
        "example"
    }

    fn subject_prefix(&self) -> &str {
        "/work/local/example"
    }

    async fn run(&self, sink: Box<dyn EventSink>) -> Result<(), AdapterError> {
        // Poll for .txt files every 5 seconds (a real adapter would use
        // filesystem notifications via the `notify` crate).
        loop {
            let entries = std::fs::read_dir(&self.watch_dir)
                .map_err(AdapterError::Io)?;

            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().map_or(false, |e| e == "txt") {
                    let content = std::fs::read_to_string(&path)
                        .map_err(AdapterError::Io)?;
                    let file_name = path.file_name()
                        .unwrap_or_default()
                        .to_string_lossy();
                    let subject = format!(
                        "{}/{}",
                        self.subject_prefix(),
                        file_name
                    );

                    tracing::info!(subject = %subject, "publishing file event");
                    sink.publish(
                        &subject,
                        "file.detected",
                        serde_json::json!({
                            "path": path.display().to_string(),
                            "content": content,
                            "size_bytes": content.len(),
                        }),
                    ).await?;
                }
            }

            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    }
}
```

### Step 3: Wire up a binary

Create `src/main.rs`:

```rust
use ctxd_adapter_core::AsyncDirectSink;
use ctxd_store::EventStore;
use std::path::PathBuf;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let store = EventStore::open(&PathBuf::from("ctxd.db")).await?;
    let sink = AsyncDirectSink::new(
        "ctxd://adapter-example".to_string(),
        Arc::new(store),
    );

    let adapter = ExampleAdapter::new(PathBuf::from("./watched"));
    adapter.run(Box::new(sink)).await?;
    Ok(())
}
```

## 4. Subject Naming Conventions

Adapters should write under a consistent prefix that identifies the source:

| Source | Prefix | Example Subject |
|--------|--------|-----------------|
| Local filesystem | `/work/local/files` | `/work/local/files/src/main.rs` |
| GitHub | `/work/github/{owner}/{repo}` | `/work/github/acme/webapp/issues/42` |
| Gmail | `/personal/gmail` | `/personal/gmail/inbox/msg-abc123` |
| Custom API | `/work/{service}` | `/work/salesforce/leads/lead-99` |

Rules:

- Start with `/work/` for work-related sources, `/personal/` for personal.
- Use lowercase, hyphens for word separation.
- Mirror the source's natural hierarchy in the path segments.
- Keep subjects stable — the same source entity should always map to the same
  subject path.

## 5. Event Type Conventions

Event types describe what happened. Use a dotted namespace:

| Event Type | When |
|------------|------|
| `file.created` | A new file appeared |
| `file.modified` | An existing file was changed |
| `file.deleted` | A file was removed |
| `issue.opened` | A GitHub issue was opened |
| `issue.commented` | A comment was added to an issue |
| `email.received` | A new email arrived |
| `ctx.note` | A user or agent created a note |

The convention is `{noun}.{verb-past-tense}`. Keep types short and consistent.

## 6. Running an Adapter Alongside the Daemon

For in-process adapters, the daemon spawns the adapter as a Tokio task sharing
the same `EventStore`. The adapter writes events through a `DirectSink` or
`AsyncDirectSink`, avoiding network round-trips.

For out-of-process adapters, run the adapter binary separately. It connects to
the daemon over the wire protocol (TCP, port 7778 by default) or the HTTP API
(port 7777):

```bash
# Terminal 1: start the daemon
ctxd serve --bind 127.0.0.1:7777

# Terminal 2: run the adapter
ctxd-adapter-fs --db ctxd.db --watch ./my-project
```

Both approaches write to the same event log. The in-process path has lower
latency; the out-of-process path allows adapters to be written in any language
and restarted independently.

## 7. Reference: The Filesystem Adapter

The `ctxd-adapter-fs` crate is the reference adapter implementation. It:

- Uses the `notify` crate to watch a directory for filesystem events.
- Maps file paths to subjects under `/work/local/files/{relative_path}`.
- Emits `file.created`, `file.modified`, and `file.deleted` event types.
- Only ingests text files (valid UTF-8) to avoid binary blobs.
- Caps file content at 100KB (`MAX_CONTENT_SIZE`) per event.
- Implements the `Adapter` trait from `ctxd-adapter-core`.

Source: `crates/ctxd-adapter-fs/src/lib.rs`

### Cargo.toml dependencies

```toml
[dependencies]
ctxd-adapter-core = { path = "../ctxd-adapter-core" }
ctxd-core = { path = "../ctxd-core" }
ctxd-store = { path = "../ctxd-store" }
serde = { workspace = true }
serde_json = { workspace = true }
tokio = { workspace = true }
tracing = { workspace = true }
async-trait = "0.1"
notify = "7"
```

The filesystem adapter is a good starting point for writing your own. Copy its
structure, replace the `notify` watcher with your source's change detection
mechanism, and adjust the subject prefix and event types.
