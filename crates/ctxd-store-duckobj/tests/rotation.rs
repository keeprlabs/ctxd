//! Rotation test: writing 2000 events should cause the buffer to
//! rotate at least twice, materializing 2+ Parquet files on disk. We
//! verify by walking the object store's `events/` prefix after the
//! writes complete.

use ctxd_core::event::Event;
use ctxd_core::subject::Subject;
use ctxd_store_duckobj::DuckObjStore;
use tempfile::TempDir;
use walkdir::WalkDir;

// walkdir isn't a direct dep; pull it in via a tiny helper instead.
mod walkdir {
    use std::path::{Path, PathBuf};

    pub struct WalkDir {
        root: PathBuf,
    }

    impl WalkDir {
        pub fn new<P: AsRef<Path>>(p: P) -> Self {
            Self {
                root: p.as_ref().to_path_buf(),
            }
        }

        pub fn into_iter(self) -> impl Iterator<Item = PathBuf> {
            walk(&self.root).into_iter()
        }
    }

    fn walk(root: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if !root.exists() {
            return out;
        }
        let mut stack = vec![root.to_path_buf()];
        while let Some(p) = stack.pop() {
            if p.is_dir() {
                if let Ok(rd) = std::fs::read_dir(&p) {
                    for entry in rd.flatten() {
                        stack.push(entry.path());
                    }
                }
            }
            out.push(p);
        }
        out
    }
}

#[tokio::test]
async fn rotation_produces_at_least_two_parquet_parts() {
    let td = TempDir::new().unwrap();
    let root = td.path().to_path_buf();

    let store = DuckObjStore::open_local(&root).await.unwrap();
    // 2000 events on a single subject root so parts all live in the
    // same partition — gives us a tight count to assert on.
    let subject = Subject::new("/bench/rotation").unwrap();
    for i in 0..2000 {
        let e = Event::new(
            "ctxd://test".to_string(),
            subject.clone(),
            "demo".to_string(),
            serde_json::json!({"i": i, "payload": "x".repeat(256)}),
        );
        store.append(e).await.unwrap();
    }
    // Make sure any stragglers below the 1000-event threshold get sealed.
    store.flush().await.unwrap();

    let mut parquet_count = 0usize;
    for p in WalkDir::new(root.join("events")).into_iter() {
        if p.extension().and_then(|s| s.to_str()) == Some("parquet") {
            let size = std::fs::metadata(&p).unwrap().len();
            assert!(
                size <= 64 * 1024 * 1024,
                "part {p:?} exceeded 64 MiB soft cap ({} bytes)",
                size
            );
            parquet_count += 1;
        }
    }
    assert!(
        parquet_count >= 2,
        "expected at least 2 rotations, got {parquet_count}"
    );
}
