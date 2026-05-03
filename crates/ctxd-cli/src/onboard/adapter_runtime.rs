//! Spawn in-process adapter tasks driven by [`skills_toml`].
//!
//! The daemon's serve loop calls [`spawn_enabled`] right after
//! HTTP/MCP setup. Each enabled adapter becomes a `tokio::spawn`'d
//! task that runs for the lifetime of the daemon, publishing into
//! the [`EventStore`] via a [`StoreSink`].
//!
//! Adapters share the daemon's store handle. They write under their
//! own subject namespace (configured per-adapter; the cap files
//! pre-minted by phase 2A scope each one to a narrow path so a
//! buggy adapter can't trample on user preferences).
//!
//! Failures from `Adapter::run` are logged but do not crash the
//! daemon. The user can re-enable an adapter by editing
//! `skills.toml` and restarting `ctxd serve`, or the doctor's
//! `adapters` check will surface the failure.

use anyhow::Result;
use async_trait::async_trait;
use ctxd_adapter_core::{Adapter, AdapterError, EventSink};
use ctxd_adapter_fs::FsAdapter;
use ctxd_core::event::Event;
use ctxd_core::subject::Subject;
use ctxd_store::EventStore;
use serde_json::Value;

use crate::onboard::skills_toml::{self, SkillsToml};

/// EventSink implementation that writes through a daemon-owned
/// [`EventStore`] handle. Cheap to clone (the store is Arc-backed).
pub struct StoreSink {
    store: EventStore,
    source: String,
}

impl StoreSink {
    pub fn new(store: EventStore, source: impl Into<String>) -> Self {
        Self {
            store,
            source: source.into(),
        }
    }
}

#[async_trait]
impl EventSink for StoreSink {
    async fn publish(
        &self,
        subject: &str,
        event_type: &str,
        data: Value,
    ) -> std::result::Result<String, AdapterError> {
        let s = Subject::new(subject)
            .map_err(|e| AdapterError::Internal(format!("invalid subject {subject}: {e}")))?;
        let event = Event::new(self.source.clone(), s, event_type.to_string(), data);
        let stored = self
            .store
            .append(event)
            .await
            .map_err(|e| AdapterError::Internal(format!("append: {e}")))?;
        Ok(stored.id.to_string())
    }
}

/// Spawn every adapter the manifest at `<path>` declares enabled.
/// Returns one `JoinHandle` per spawned adapter so the caller can
/// track them alongside the wire/HTTP server handles. Empty vec on
/// no manifest / nothing enabled.
pub fn spawn_enabled(
    store: &EventStore,
    manifest_path: &std::path::Path,
) -> Result<Vec<tokio::task::JoinHandle<()>>> {
    let manifest = skills_toml::read_at(manifest_path)?;
    spawn_from_manifest(store, &manifest)
}

/// Same as [`spawn_enabled`] but takes a pre-loaded manifest. Useful
/// for tests and callers that read the manifest themselves.
pub fn spawn_from_manifest(
    store: &EventStore,
    manifest: &SkillsToml,
) -> Result<Vec<tokio::task::JoinHandle<()>>> {
    let mut handles = Vec::new();

    if let Some(fs) = &manifest.fs {
        if fs.enabled && !fs.paths.is_empty() {
            for path in &fs.paths {
                let path = path.clone();
                let store = store.clone();
                let handle = tokio::spawn(async move {
                    tracing::info!(path = %path.to_string_lossy(), "fs adapter starting");
                    let adapter = FsAdapter::new(path.clone());
                    let sink: Box<dyn EventSink> =
                        Box::new(StoreSink::new(store, "ctxd://adapter/fs"));
                    if let Err(e) = adapter.run(sink).await {
                        tracing::error!(
                            path = %path.to_string_lossy(),
                            error = %e,
                            "fs adapter task ended with error"
                        );
                    }
                });
                handles.push(handle);
            }
        }
    }

    // Gmail and GitHub adapters live in their own crates with the
    // same Adapter trait shape but require token storage + OAuth /
    // PAT plumbing that's still being wired (phase 3B follow-on).
    // The skills.toml shape is stable today — when the adapters are
    // ready, this is the only call site that needs to change.
    if let Some(_gmail) = &manifest.gmail {
        // Phase 3B follow-on: spawn ctxd_adapter_gmail::GmailAdapter
        // when token_file exists and is decryptable. For now we no-op
        // with a hint in the log so users see we read the manifest.
        tracing::info!("gmail adapter declared in skills.toml; spawn deferred to next 3B drop");
    }
    if let Some(_github) = &manifest.github {
        tracing::info!("github adapter declared in skills.toml; spawn deferred to next 3B drop");
    }

    Ok(handles)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn store_sink_publishes_event() {
        let dir = tempfile::tempdir().unwrap();
        let store = EventStore::open(&dir.path().join("ctxd.db")).await.unwrap();
        let sink = StoreSink::new(store.clone(), "ctxd://test");
        let id = sink
            .publish(
                "/me/test",
                "test.event",
                serde_json::json!({"hello": "world"}),
            )
            .await
            .expect("publish");
        assert!(!id.is_empty());
        // Verify the event landed.
        let s = Subject::new("/me/test").unwrap();
        let events = store.read(&s, false).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "test.event");
    }

    #[tokio::test]
    async fn spawn_from_manifest_with_no_fs_paths_returns_zero_handles() {
        let dir = tempfile::tempdir().unwrap();
        let store = EventStore::open(&dir.path().join("ctxd.db")).await.unwrap();
        // Manifest with fs.enabled=true but empty paths list.
        let manifest = SkillsToml {
            fs: Some(skills_toml::FsSkill {
                enabled: true,
                paths: vec![],
            }),
            gmail: None,
            github: None,
        };
        let handles = spawn_from_manifest(&store, &manifest).unwrap();
        assert!(handles.is_empty(), "no paths → no handles");
    }

    #[tokio::test]
    async fn spawn_from_manifest_with_disabled_fs_returns_zero_handles() {
        let dir = tempfile::tempdir().unwrap();
        let store = EventStore::open(&dir.path().join("ctxd.db")).await.unwrap();
        let manifest = SkillsToml {
            fs: Some(skills_toml::FsSkill {
                enabled: false,
                paths: vec!["/tmp/x".into()],
            }),
            gmail: None,
            github: None,
        };
        let handles = spawn_from_manifest(&store, &manifest).unwrap();
        assert!(handles.is_empty(), "disabled → no handles");
    }

    #[tokio::test]
    async fn spawn_from_manifest_with_enabled_fs_spawns_one_handle_per_path() {
        // Use real (but empty) tempdirs as watch targets so the
        // notify watcher initializes cleanly without the test ever
        // generating events.
        let dir = tempfile::tempdir().unwrap();
        let watch1 = tempfile::tempdir().unwrap();
        let watch2 = tempfile::tempdir().unwrap();
        let store = EventStore::open(&dir.path().join("ctxd.db")).await.unwrap();
        let manifest = SkillsToml {
            fs: Some(skills_toml::FsSkill {
                enabled: true,
                paths: vec![watch1.path().to_path_buf(), watch2.path().to_path_buf()],
            }),
            gmail: None,
            github: None,
        };
        let handles = spawn_from_manifest(&store, &manifest).unwrap();
        assert_eq!(handles.len(), 2);
        // Tear down: abort handles + drop tempdirs.
        for h in handles {
            h.abort();
        }
    }
}
