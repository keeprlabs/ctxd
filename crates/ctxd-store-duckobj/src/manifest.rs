//! Manifest: the list of sealed Parquet parts visible to readers.
//!
//! ## Format
//!
//! JSON file at `<prefix>/events/_manifest.json` with the shape:
//!
//! ```json
//! {
//!   "version": 1,
//!   "parts": [
//!     {
//!       "path": "events/work/2025-01/part-00000000000000000001-<uuid>.parquet",
//!       "subject_root": "work",
//!       "yyyy_mm": "2025-01",
//!       "min_seq": 1,
//!       "max_seq": 1000,
//!       "min_time": "2025-01-01T00:00:00Z",
//!       "max_time": "2025-01-01T00:00:01Z",
//!       "event_count": 1000
//!     }
//!   ]
//! }
//! ```
//!
//! ## Atomicity
//!
//! Writing the manifest is the integrity boundary for the store. We
//! always write the full JSON document as a single object-store PUT.
//! Most object stores (S3, R2, Azure Blob, GCS) treat a PUT as atomic
//! at the object level — partial writes are never visible. For plain
//! local filesystems, [`object_store::local::LocalFileSystem`] uses
//! atomic rename.
//!
//! See ADR 018 for the concurrency analysis. Two writers racing the
//! manifest are serialized by the outer [`crate::store::DuckObjStore`]
//! mutex on `manifest`; this module assumes serial access.

use std::collections::{BTreeMap, BTreeSet};

use bytes::Bytes;
use chrono::{DateTime, Utc};
use object_store::path::Path as ObjPath;
use object_store::{ObjectStore, ObjectStoreExt, PutPayload};
use serde::{Deserialize, Serialize};

use crate::store::StoreError;

/// Schema version for the manifest JSON. Bumping this version is a
/// breaking change — old daemons will refuse to open newer manifests.
pub const MANIFEST_VERSION: u32 = 1;

/// A single sealed Parquet part as recorded in the manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartMeta {
    /// Object-store path relative to the store prefix.
    pub path: String,
    /// First path segment of the subject tree this part contains.
    pub subject_root: String,
    /// `YYYY-MM` partition key.
    pub yyyy_mm: String,
    /// Minimum seq in the part (inclusive).
    pub min_seq: i64,
    /// Maximum seq in the part (inclusive).
    pub max_seq: i64,
    /// Minimum event timestamp in the part.
    pub min_time: DateTime<Utc>,
    /// Maximum event timestamp in the part.
    pub max_time: DateTime<Utc>,
    /// Number of rows in the part.
    pub event_count: u64,
}

/// The on-disk manifest, plus helpers.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Manifest {
    /// Schema version.
    pub version: u32,
    /// Sealed parts, in insertion (seal) order.
    pub parts: Vec<PartMeta>,
}

impl Manifest {
    /// Construct an empty manifest (no parts yet).
    pub fn empty() -> Self {
        Self {
            version: MANIFEST_VERSION,
            parts: Vec::new(),
        }
    }

    /// Load a manifest from the object store. Returns [`Manifest::empty`]
    /// if the manifest object does not exist. Ignores malformed
    /// trailing bytes (partial writes) by stripping to the last
    /// balanced JSON object — see [`parse_best_effort`].
    pub async fn load(store: &dyn ObjectStore, path: &ObjPath) -> Result<Self, StoreError> {
        match store.get(path).await {
            Ok(res) => {
                let bytes = res.bytes().await?;
                let parsed = parse_best_effort(&bytes)?;
                Ok(parsed)
            }
            Err(object_store::Error::NotFound { .. }) => Ok(Self::empty()),
            Err(e) => Err(StoreError::ObjectStore(e)),
        }
    }

    /// Persist the manifest with a single PUT. Callers should hold
    /// an outer mutex so concurrent writers don't interleave; this
    /// method does not attempt to serialize writers itself.
    pub async fn save(&self, store: &dyn ObjectStore, path: &ObjPath) -> Result<(), StoreError> {
        let bytes = serde_json::to_vec_pretty(self)?;
        store
            .put(path, PutPayload::from(Bytes::from(bytes)))
            .await?;
        Ok(())
    }

    /// Add a part. Dedups by `path` so replaying a flush is a no-op.
    pub fn add_part(&mut self, meta: PartMeta) {
        if !self.parts.iter().any(|p| p.path == meta.path) {
            self.parts.push(meta);
        }
    }

    /// Clone-out the full list of parts for readers. Cheap — each
    /// `PartMeta` is a handful of owned strings + fixed-size fields.
    pub fn parts_snapshot(&self) -> Vec<PartMeta> {
        self.parts.clone()
    }

    /// Parts whose `subject_root` matches the first segment of
    /// `subject`. Conservative: returns all parts if the caller can't
    /// be pinned to a root.
    pub fn parts_for_subject(&self, subject: &str) -> Vec<PartMeta> {
        let first = subject
            .trim_start_matches('/')
            .split('/')
            .next()
            .unwrap_or("");
        let root = if first.is_empty() { "_root" } else { first };
        self.parts
            .iter()
            .filter(|p| p.subject_root == root)
            .cloned()
            .collect()
    }

    /// The highest seq present across all sealed parts.
    pub fn max_seq(&self) -> Option<i64> {
        self.parts.iter().map(|p| p.max_seq).max()
    }

    /// Unique subject roots known to the manifest. Handy for
    /// backfill / garbage-collection tooling.
    pub fn subject_roots(&self) -> BTreeSet<String> {
        self.parts.iter().map(|p| p.subject_root.clone()).collect()
    }

    /// Parts grouped by `(subject_root, yyyy_mm)`. Preserves input
    /// order within each bucket.
    pub fn parts_by_partition(&self) -> BTreeMap<(String, String), Vec<PartMeta>> {
        let mut out: BTreeMap<(String, String), Vec<PartMeta>> = BTreeMap::new();
        for p in &self.parts {
            out.entry((p.subject_root.clone(), p.yyyy_mm.clone()))
                .or_default()
                .push(p.clone());
        }
        out
    }
}

/// Parse a manifest, tolerating trailing garbage from a torn write.
///
/// Strategy: try a full `serde_json::from_slice` first; on failure,
/// find the last `}` byte and try parsing the prefix. This rescues
/// the common "PUT aborted half-way and left extra bytes appended"
/// case on filesystems that don't honour atomic rename (no-op on
/// cloud object stores where PUT is atomic at the whole-object level).
fn parse_best_effort(bytes: &Bytes) -> Result<Manifest, StoreError> {
    if let Ok(m) = serde_json::from_slice::<Manifest>(bytes) {
        return Ok(m);
    }
    // Find last '}' and try again.
    if let Some(idx) = bytes.iter().rposition(|b| *b == b'}') {
        let slice = &bytes[..=idx];
        if let Ok(m) = serde_json::from_slice::<Manifest>(slice) {
            tracing::warn!(
                trimmed = bytes.len() - slice.len(),
                "manifest had trailing garbage; parsed the last valid prefix"
            );
            return Ok(m);
        }
    }
    Err(StoreError::Decode(
        "manifest JSON could not be parsed".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(path: &str, root: &str, min: i64, max: i64) -> PartMeta {
        PartMeta {
            path: path.to_string(),
            subject_root: root.to_string(),
            yyyy_mm: "2025-01".to_string(),
            min_seq: min,
            max_seq: max,
            min_time: Utc::now(),
            max_time: Utc::now(),
            event_count: (max - min + 1) as u64,
        }
    }

    #[test]
    fn add_part_is_idempotent() {
        let mut m = Manifest::empty();
        m.add_part(mk("a.parquet", "r", 1, 10));
        m.add_part(mk("a.parquet", "r", 1, 10));
        assert_eq!(m.parts.len(), 1);
    }

    #[test]
    fn max_seq_is_maximum_across_parts() {
        let mut m = Manifest::empty();
        m.add_part(mk("a.parquet", "r", 1, 10));
        m.add_part(mk("b.parquet", "r", 11, 20));
        assert_eq!(m.max_seq(), Some(20));
    }

    #[test]
    fn parse_best_effort_trims_trailing_garbage() {
        let m = Manifest::empty();
        let mut bytes = serde_json::to_vec(&m).unwrap();
        bytes.extend_from_slice(b"\x00\x00");
        let b = Bytes::from(bytes);
        let parsed = parse_best_effort(&b).unwrap();
        assert_eq!(parsed.version, MANIFEST_VERSION);
    }

    #[test]
    fn parts_for_subject_resolves_root_segment() {
        let mut m = Manifest::empty();
        m.add_part(mk("w.parquet", "work", 1, 5));
        m.add_part(mk("h.parquet", "home", 6, 10));
        let work = m.parts_for_subject("/work/foo");
        assert_eq!(work.len(), 1);
        assert_eq!(work[0].subject_root, "work");
    }
}
