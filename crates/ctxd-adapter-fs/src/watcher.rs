//! Filesystem watcher implementation.

use std::path::{Path, PathBuf};

use ctxd_adapter_core::{Adapter, AdapterError, EventSink};
use notify::{Event as NotifyEvent, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::MAX_CONTENT_SIZE;

/// Adapter that watches a filesystem directory and publishes file change events.
pub struct FsAdapter {
    /// The directory to watch.
    watch_dir: PathBuf,
}

impl FsAdapter {
    /// Create a new filesystem adapter.
    ///
    /// # Arguments
    /// * `watch_dir` - The directory to recursively watch for changes
    pub fn new(watch_dir: PathBuf) -> Self {
        Self { watch_dir }
    }

    /// Compute a relative path from the watch directory and convert to a subject.
    fn relative_subject(&self, path: &Path) -> Option<String> {
        let rel = path.strip_prefix(&self.watch_dir).ok()?;
        let rel_str = rel.to_str()?;
        // Normalize path separators to forward slashes
        let normalized = rel_str.replace('\\', "/");
        Some(format!("/work/local/files/{normalized}"))
    }

    /// Read a file and build event data, returning None for binary files or files that are too large.
    async fn read_file_data(path: &Path) -> Option<serde_json::Value> {
        let metadata = tokio::fs::metadata(path).await.ok()?;

        if !metadata.is_file() {
            return None;
        }

        let size = metadata.len();

        // Skip files larger than the content cap
        if size > MAX_CONTENT_SIZE {
            debug!(?path, size, "skipping file: exceeds 100KB content cap");
            return None;
        }

        let bytes = tokio::fs::read(path).await.ok()?;

        // Check if content is valid UTF-8 (skip binary files)
        let content = match String::from_utf8(bytes) {
            Ok(s) => s,
            Err(_) => {
                debug!(?path, "skipping file: not valid UTF-8 (binary)");
                return None;
            }
        };

        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();

        let modified = metadata
            .modified()
            .ok()
            .map(|t| {
                let dt: chrono::DateTime<chrono::Utc> = t.into();
                dt.to_rfc3339()
            })
            .unwrap_or_default();

        Some(serde_json::json!({
            "path": path.to_string_lossy(),
            "name": name,
            "size": size,
            "content": content,
            "modified": modified,
        }))
    }
}

#[async_trait::async_trait]
impl Adapter for FsAdapter {
    fn name(&self) -> &str {
        "filesystem"
    }

    fn subject_prefix(&self) -> &str {
        "/work/local"
    }

    async fn run(&self, sink: Box<dyn EventSink>) -> Result<(), AdapterError> {
        info!(dir = %self.watch_dir.display(), "starting filesystem adapter");

        // Use an unbounded tokio channel. The sender's `send()` method does
        // not require an async runtime, so it works from the OS thread that
        // notify uses for its callback.
        let (tx, mut rx) = mpsc::unbounded_channel::<NotifyEvent>();

        let _watcher = {
            let mut w = RecommendedWatcher::new(
                move |res: Result<NotifyEvent, notify::Error>| {
                    if let Ok(event) = res {
                        let _ = tx.send(event);
                    }
                },
                notify::Config::default(),
            )
            .map_err(|e| AdapterError::Internal(format!("failed to create watcher: {e}")))?;

            w.watch(&self.watch_dir, RecursiveMode::Recursive)
                .map_err(|e| AdapterError::Internal(format!("failed to watch directory: {e}")))?;

            w
        };

        info!(dir = %self.watch_dir.display(), "watching for file changes");

        while let Some(event) = rx.recv().await {
            for path in &event.paths {
                let subject = match self.relative_subject(path) {
                    Some(s) => s,
                    None => {
                        debug!(?path, "skipping path outside watch directory");
                        continue;
                    }
                };

                match event.kind {
                    EventKind::Create(_) => {
                        if let Some(data) = Self::read_file_data(path).await {
                            match sink.publish(&subject, "file.created", data).await {
                                Ok(id) => debug!(id, subject, "published file.created"),
                                Err(e) => warn!(%e, subject, "failed to publish file.created"),
                            }
                        }
                    }
                    EventKind::Modify(_) => {
                        if let Some(data) = Self::read_file_data(path).await {
                            match sink.publish(&subject, "file.modified", data).await {
                                Ok(id) => debug!(id, subject, "published file.modified"),
                                Err(e) => warn!(%e, subject, "failed to publish file.modified"),
                            }
                        }
                    }
                    EventKind::Remove(_) => {
                        let data = serde_json::json!({
                            "path": path.to_string_lossy(),
                        });
                        match sink.publish(&subject, "file.deleted", data).await {
                            Ok(id) => debug!(id, subject, "published file.deleted"),
                            Err(e) => warn!(%e, subject, "failed to publish file.deleted"),
                        }
                    }
                    _ => {
                        // Ignore other event kinds (access, etc.)
                    }
                }
            }
        }

        // Channel closed - watcher was dropped
        info!("filesystem watcher stopped");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctxd_adapter_core::EventSink;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    /// Collected event: (subject, event_type, data).
    type CollectedEvent = (String, String, serde_json::Value);

    /// A test sink that collects published events.
    struct CollectingSink {
        events: Arc<Mutex<Vec<CollectedEvent>>>,
    }

    #[async_trait::async_trait]
    impl EventSink for CollectingSink {
        async fn publish(
            &self,
            subject: &str,
            event_type: &str,
            data: serde_json::Value,
        ) -> Result<String, AdapterError> {
            let mut events = self.events.lock().await;
            events.push((subject.to_string(), event_type.to_string(), data));
            Ok(uuid::Uuid::now_v7().to_string())
        }
    }

    /// Poll until the predicate returns true on the collected events, or time out.
    async fn wait_for_events(
        events: &Arc<Mutex<Vec<CollectedEvent>>>,
        predicate: impl Fn(&[CollectedEvent]) -> bool,
    ) -> bool {
        for _ in 0..60 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let collected = events.lock().await;
            if predicate(&collected) {
                return true;
            }
        }
        false
    }

    #[tokio::test]
    #[ignore = "flaky: depends on OS filesystem watcher timing"]
    async fn fs_adapter_publishes_file_created() {
        let tmp = tempfile::tempdir().unwrap();
        let adapter = FsAdapter::new(tmp.path().to_path_buf());
        let events: Arc<Mutex<Vec<CollectedEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = CollectingSink {
            events: events.clone(),
        };

        // Run the adapter in a background task
        let adapter_handle = tokio::spawn(async move { adapter.run(Box::new(sink)).await });

        // Give the watcher time to start
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Write a test file
        let test_file = tmp.path().join("hello.txt");
        tokio::fs::write(&test_file, "Hello, world!").await.unwrap();

        // Poll until we see the event
        let found = wait_for_events(&events, |collected| {
            collected.iter().any(|(s, _, _)| s.contains("hello.txt"))
        })
        .await;
        assert!(found, "expected an event for hello.txt within timeout");

        let collected = events.lock().await;
        let file_event = collected
            .iter()
            .find(|(s, _, _)| s.contains("hello.txt"))
            .unwrap();

        assert!(
            file_event.1 == "file.created" || file_event.1 == "file.modified",
            "expected file.created or file.modified, got {}",
            file_event.1
        );

        assert_eq!(file_event.2["name"], "hello.txt");
        assert_eq!(file_event.2["content"], "Hello, world!");
        assert_eq!(file_event.0, "/work/local/files/hello.txt");

        adapter_handle.abort();
    }

    #[tokio::test]
    #[ignore = "flaky: depends on OS filesystem watcher timing"]
    async fn fs_adapter_publishes_file_deleted() {
        let tmp = tempfile::tempdir().unwrap();

        // Create file before starting the adapter
        let test_file = tmp.path().join("to_delete.txt");
        tokio::fs::write(&test_file, "bye").await.unwrap();

        let adapter = FsAdapter::new(tmp.path().to_path_buf());
        let events: Arc<Mutex<Vec<CollectedEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = CollectingSink {
            events: events.clone(),
        };

        let adapter_handle = tokio::spawn(async move { adapter.run(Box::new(sink)).await });

        // Give the watcher time to start
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Delete the file
        tokio::fs::remove_file(&test_file).await.unwrap();

        // Poll until we see the delete event
        let found = wait_for_events(&events, |collected| {
            collected.iter().any(|(_, t, _)| t == "file.deleted")
        })
        .await;
        assert!(found, "expected a file.deleted event within timeout");

        let collected = events.lock().await;
        let (subject, _, data) = collected
            .iter()
            .find(|(_, t, _)| t == "file.deleted")
            .unwrap();
        assert_eq!(subject, "/work/local/files/to_delete.txt");
        assert!(data["path"].as_str().unwrap().contains("to_delete.txt"));

        adapter_handle.abort();
    }

    #[test]
    fn relative_subject_computation() {
        let adapter = FsAdapter::new(PathBuf::from("/home/user/docs"));
        let subject = adapter
            .relative_subject(Path::new("/home/user/docs/notes/todo.md"))
            .unwrap();
        assert_eq!(subject, "/work/local/files/notes/todo.md");
    }

    #[test]
    fn adapter_trait_impl() {
        let adapter = FsAdapter::new(PathBuf::from("/tmp"));
        assert_eq!(adapter.name(), "filesystem");
        assert_eq!(adapter.subject_prefix(), "/work/local");
    }
}
