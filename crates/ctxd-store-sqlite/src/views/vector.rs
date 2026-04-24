//! Persisted HNSW vector index for ctxd embeddings.
//!
//! This module owns the in-memory `Hnsw` graph, the on-disk
//! sidecar files (`<db>.hnsw.graph` + `<db>.hnsw.data`) produced by
//! `hnsw_rs`, the `event_id <-> internal id` mapping, and our own
//! `<db>.hnsw.meta` magic + version sidecar.
//!
//! ## Design
//!
//! Embeddings are stored canonically in the `vector_embeddings`
//! SQLite table — the source of truth. The HNSW graph is a
//! *materialized* view: lost or corrupt files can always be rebuilt
//! by replaying the table.
//!
//! ## On-disk layout
//!
//! Given a database path of `<db>` we write four files alongside it:
//!
//! - `<db>.hnsw.graph` — `hnsw_rs` adjacency lists.
//! - `<db>.hnsw.data` — `hnsw_rs` raw vector payloads.
//! - `<db>.hnsw.meta` — our magic header (`CTXDVEC1` + version byte + JSON: dim, element_count). A torn write or version mismatch triggers a rebuild.
//! - `<db>.hnsw.map` — JSON `Vec<String>` of internal-id → event_id.
//!
//! ## Persistence cadence
//!
//! `dump` is called:
//! 1. On graceful shutdown via [`VectorIndex::flush`].
//! 2. Every `flush_every_n_inserts` inserts (default: 1000).
//!
//! Between flushes, a hard kill loses the most recent inserts from
//! the on-disk graph. On reopen we detect the discrepancy by
//! comparing the graph's element count against the SQL row count,
//! and we rebuild from SQL when they disagree (or when load fails).
//!
//! ## Concurrency
//!
//! `hnsw_rs` permits concurrent searches and is internally
//! synchronized for inserts; we wrap it in our own `RwLock` to keep
//! our `id_to_event` map and the graph in lockstep. Searches grab a
//! read lock; writes grab a write lock. `dump` runs under a write
//! lock — at 100k vectors this takes ~few hundred ms.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use hnsw_rs::api::AnnT;
use hnsw_rs::hnsw::Hnsw;
use hnsw_rs::hnswio::HnswIo;
use hnsw_rs::prelude::DistCosine;
use serde::{Deserialize, Serialize};

/// HNSW M parameter — max out-degree in the graph. 16 is the value
/// recommended by the HNSW paper for "general workloads"; higher
/// values trade memory for recall.
pub const HNSW_M: usize = 16;

/// HNSW efConstruction parameter. 200 keeps build quality high
/// without runaway insert latency at our expected (≤1M) corpus
/// sizes. See `docs/decisions/014-hnsw-persistence.md`.
pub const HNSW_EF_CONSTRUCTION: usize = 200;

/// HNSW efSearch parameter — controls recall vs. latency at query
/// time. 50 is conservative for k≤50 on typical data.
pub const HNSW_EF_SEARCH: usize = 50;

/// Default number of inserts between automatic flushes to disk.
pub const DEFAULT_FLUSH_EVERY_N_INSERTS: u32 = 1000;

/// Magic bytes prefixing our `<db>.hnsw.meta` file. A torn or
/// foreign file fails the magic check and triggers a rebuild.
const META_MAGIC: &[u8; 8] = b"CTXDVEC1";

/// Bumped whenever the on-disk format (or HNSW parameters baked
/// into a saved graph) becomes incompatible with this code.
const META_VERSION: u8 = 1;

/// Errors from vector view operations.
#[derive(Debug, thiserror::Error)]
pub enum VectorError {
    /// Embedding dimension does not match the index's configured dimensions.
    #[error("dimension mismatch: expected {expected}, got {got}")]
    DimensionMismatch {
        /// Expected dimensionality.
        expected: usize,
        /// Actual dimensionality provided.
        got: usize,
    },
    /// Filesystem error during load/dump.
    #[error("vector index io: {0}")]
    Io(String),
    /// HNSW backend error (load failure, internal corruption).
    #[error("vector index backend: {0}")]
    Backend(String),
    /// Concurrency lock poisoned (a previous holder panicked).
    #[error("vector index lock poisoned")]
    LockPoisoned,
}

/// Persistence + index parameters supplied at open time.
#[derive(Debug, Clone)]
pub struct VectorIndexConfig {
    /// Expected vector dimensionality. Mismatches are rejected at
    /// insert + search.
    pub dimensions: usize,
    /// Auto-flush cadence: dump after this many inserts.
    pub flush_every_n_inserts: u32,
    /// Maximum number of elements the index can hold. `hnsw_rs`
    /// requires an upfront cap; we default to 1M which is plenty
    /// for v0.3 single-node deployments.
    pub max_elements: usize,
    /// Maximum graph layer (`max_layer` in HNSW). 16 matches the
    /// hnsw_rs example for ~1M elements.
    pub max_nb_layers: usize,
}

impl Default for VectorIndexConfig {
    fn default() -> Self {
        Self {
            dimensions: 384,
            flush_every_n_inserts: DEFAULT_FLUSH_EVERY_N_INSERTS,
            max_elements: 1_000_000,
            max_nb_layers: 16,
        }
    }
}

/// Sidecar metadata persisted alongside the HNSW dump. We use it to
/// validate dimension + version compatibility before handing the
/// graph to `hnsw_rs::HnswIo::load_hnsw`.
#[derive(Serialize, Deserialize)]
struct IndexMeta {
    version: u8,
    dimensions: usize,
    element_count: usize,
}

/// Internal mutable state guarded by a RwLock.
struct Inner {
    /// The HNSW graph.
    hnsw: Hnsw<'static, f32, DistCosine>,
    /// Internal-id (the integer id we hand to `hnsw_rs`) -> event_id.
    id_to_event: Vec<String>,
    /// Inverse: event_id -> internal id. Used for upsert detection.
    event_to_id: HashMap<String, usize>,
    /// Number of inserts since the last flush. Resets on flush.
    inserts_since_flush: u32,
}

/// A persisted HNSW index handle. Cheap to clone — wraps `Arc`.
pub struct VectorIndex {
    inner: RwLock<Inner>,
    cfg: VectorIndexConfig,
    /// Directory + basename pair to use with hnsw_rs's
    /// `file_dump` / `HnswIo::new`. `None` when the index is
    /// purely in-memory (no persistence side effects).
    storage: Option<Storage>,
}

#[derive(Clone, Debug)]
struct Storage {
    /// Directory containing the dump artifacts.
    dir: PathBuf,
    /// Basename — hnsw_rs will append `.hnsw.graph` / `.hnsw.data`.
    /// Our sidecars use `<basename>.hnsw.meta` / `.hnsw.map`.
    basename: String,
}

impl Storage {
    fn graph_path(&self) -> PathBuf {
        self.dir.join(format!("{}.hnsw.graph", self.basename))
    }
    fn data_path(&self) -> PathBuf {
        self.dir.join(format!("{}.hnsw.data", self.basename))
    }
    fn meta_path(&self) -> PathBuf {
        self.dir.join(format!("{}.hnsw.meta", self.basename))
    }
    fn map_path(&self) -> PathBuf {
        self.dir.join(format!("{}.hnsw.map", self.basename))
    }
}

impl std::fmt::Debug for VectorIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VectorIndex")
            .field("dimensions", &self.cfg.dimensions)
            .field("storage", &self.storage)
            .finish()
    }
}

impl VectorIndex {
    /// Build an empty in-memory index with no on-disk persistence.
    pub fn open_in_memory(cfg: VectorIndexConfig) -> Self {
        let hnsw = Self::new_hnsw(&cfg);
        Self {
            inner: RwLock::new(Inner {
                hnsw,
                id_to_event: Vec::new(),
                event_to_id: HashMap::new(),
                inserts_since_flush: 0,
            }),
            cfg,
            storage: None,
        }
    }

    /// Open (or create) a persistent index using `<db>.hnsw.*` files.
    ///
    /// `db_path` is the SQLite database path; we derive the dump
    /// basename + directory from it. If a valid graph + meta exist
    /// on disk and match the configured dimensions, they're loaded.
    /// Otherwise the caller follows up with [`VectorIndex::rebuild_from`]
    /// to repopulate the index from the SQL table.
    pub fn open_persistent(
        db_path: &Path,
        cfg: VectorIndexConfig,
    ) -> Result<(Self, OpenStatus), VectorError> {
        let dir = db_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let basename = db_path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| VectorError::Io("invalid db path".to_string()))?
            .to_string();
        let storage = Storage { dir, basename };
        match Self::try_load(&storage, &cfg) {
            Ok(inner) => Ok((
                Self {
                    inner: RwLock::new(inner),
                    cfg,
                    storage: Some(storage),
                },
                OpenStatus::LoadedFromDisk,
            )),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "vector index load failed; starting empty (caller should rebuild from SQL)"
                );
                let hnsw = Self::new_hnsw(&cfg);
                Ok((
                    Self {
                        inner: RwLock::new(Inner {
                            hnsw,
                            id_to_event: Vec::new(),
                            event_to_id: HashMap::new(),
                            inserts_since_flush: 0,
                        }),
                        cfg,
                        storage: Some(storage),
                    },
                    OpenStatus::RebuildRequired,
                ))
            }
        }
    }

    fn new_hnsw(cfg: &VectorIndexConfig) -> Hnsw<'static, f32, DistCosine> {
        Hnsw::<f32, DistCosine>::new(
            HNSW_M,
            cfg.max_elements,
            cfg.max_nb_layers,
            HNSW_EF_CONSTRUCTION,
            DistCosine,
        )
    }

    fn try_load(storage: &Storage, cfg: &VectorIndexConfig) -> Result<Inner, VectorError> {
        if !storage.meta_path().exists()
            || !storage.graph_path().exists()
            || !storage.data_path().exists()
            || !storage.map_path().exists()
        {
            return Err(VectorError::Io(
                "one or more index sidecar files missing".to_string(),
            ));
        }
        // Validate meta (magic + version + dim) before touching the
        // potentially-large graph file.
        let mut meta_bytes = Vec::new();
        let mut f = fs::File::open(storage.meta_path())
            .map_err(|e| VectorError::Io(format!("open meta: {e}")))?;
        f.read_to_end(&mut meta_bytes)
            .map_err(|e| VectorError::Io(format!("read meta: {e}")))?;
        if meta_bytes.len() < META_MAGIC.len() + 1 {
            return Err(VectorError::Io("meta too short".to_string()));
        }
        if &meta_bytes[..META_MAGIC.len()] != META_MAGIC {
            return Err(VectorError::Io("meta magic mismatch".to_string()));
        }
        let ver = meta_bytes[META_MAGIC.len()];
        if ver != META_VERSION {
            return Err(VectorError::Io(format!(
                "meta version {ver} != expected {META_VERSION}"
            )));
        }
        let json_bytes = &meta_bytes[META_MAGIC.len() + 1..];
        let meta: IndexMeta = serde_json::from_slice(json_bytes)
            .map_err(|e| VectorError::Io(format!("meta parse: {e}")))?;
        if meta.dimensions != cfg.dimensions {
            return Err(VectorError::DimensionMismatch {
                expected: cfg.dimensions,
                got: meta.dimensions,
            });
        }

        // Load HNSW. The returned `Hnsw` borrows from the `HnswIo`,
        // so we leak the loader to grant it `'static`. There is at
        // most one leak per process lifetime per index instance —
        // reloads happen on startup and on rebuild, both rare.
        let hio: &'static mut HnswIo =
            Box::leak(Box::new(HnswIo::new(&storage.dir, &storage.basename)));
        let hnsw: Hnsw<'static, f32, DistCosine> = hio
            .load_hnsw()
            .map_err(|e| VectorError::Backend(format!("load_hnsw: {e}")))?;

        // Load id<->event map.
        let map_bytes =
            fs::read(storage.map_path()).map_err(|e| VectorError::Io(format!("read map: {e}")))?;
        let id_to_event: Vec<String> = serde_json::from_slice(&map_bytes)
            .map_err(|e| VectorError::Io(format!("map parse: {e}")))?;
        let event_to_id: HashMap<String, usize> = id_to_event
            .iter()
            .enumerate()
            .map(|(i, s)| (s.clone(), i))
            .collect();
        let count = id_to_event.len();
        if count != meta.element_count {
            return Err(VectorError::Io(format!(
                "meta element_count {} != map len {}",
                meta.element_count, count
            )));
        }
        tracing::info!(elements = count, "loaded persisted HNSW index");
        Ok(Inner {
            hnsw,
            id_to_event,
            event_to_id,
            inserts_since_flush: 0,
        })
    }

    /// Insert (or update) an embedding for `event_id`.
    ///
    /// `hnsw_rs` does not support in-place removal, so a duplicate
    /// `event_id` causes a second graph entry — but the
    /// `event_to_id` map is updated to point at the latest, and the
    /// SQL `vector_embeddings` table holds the canonical latest
    /// vector. Search results dedupe by event_id at the caller.
    pub fn upsert(&self, event_id: &str, vector: &[f32]) -> Result<(), VectorError> {
        if vector.len() != self.cfg.dimensions {
            return Err(VectorError::DimensionMismatch {
                expected: self.cfg.dimensions,
                got: vector.len(),
            });
        }
        let mut inner = self.inner.write().map_err(|_| VectorError::LockPoisoned)?;
        let owned = vector.to_vec();
        let id = inner.id_to_event.len();
        inner.hnsw.insert((&owned, id));
        inner.id_to_event.push(event_id.to_string());
        inner.event_to_id.insert(event_id.to_string(), id);
        inner.inserts_since_flush = inner.inserts_since_flush.saturating_add(1);
        let should_flush = inner.inserts_since_flush >= self.cfg.flush_every_n_inserts;
        if should_flush && self.storage.is_some() {
            if let Err(e) = self.dump_locked(&mut inner) {
                tracing::warn!(error = %e, "vector index auto-flush failed; will retry on next flush");
            } else {
                inner.inserts_since_flush = 0;
            }
        }
        Ok(())
    }

    /// k-nearest-neighbor search. Returns `(event_id, distance)` pairs
    /// sorted by ascending cosine distance. Duplicates from upserts
    /// are deduped here so callers see at most one row per event_id.
    pub fn search(&self, query: &[f32], k: usize) -> Result<Vec<(String, f32)>, VectorError> {
        if query.len() != self.cfg.dimensions {
            return Err(VectorError::DimensionMismatch {
                expected: self.cfg.dimensions,
                got: query.len(),
            });
        }
        if k == 0 {
            return Ok(Vec::new());
        }
        let inner = self.inner.read().map_err(|_| VectorError::LockPoisoned)?;
        // Ask the index for more than k so we can dedupe and still
        // hit k unique event_ids in the common no-dup case.
        let neighbors = inner
            .hnsw
            .search(query, k.saturating_mul(2), HNSW_EF_SEARCH);
        let mut out = Vec::with_capacity(neighbors.len());
        let mut seen: HashMap<&str, ()> = HashMap::with_capacity(neighbors.len());
        for n in &neighbors {
            let event_id = match inner.id_to_event.get(n.d_id) {
                Some(s) => s.as_str(),
                None => {
                    return Err(VectorError::Backend(format!(
                        "unknown internal id {}",
                        n.d_id
                    )))
                }
            };
            if seen.insert(event_id, ()).is_none() {
                out.push((event_id.to_string(), n.distance));
                if out.len() >= k {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// Number of distinct event_ids currently in the index.
    pub fn len(&self) -> usize {
        self.inner.read().map(|g| g.event_to_id.len()).unwrap_or(0)
    }

    /// True iff the index holds no vectors.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Configured dimensionality.
    pub fn dimensions(&self) -> usize {
        self.cfg.dimensions
    }

    /// Drop the in-memory state and rebuild from a stream of
    /// `(event_id, vector)` pairs (typically the SQL table).
    pub fn rebuild_from<'a>(
        &self,
        iter: impl IntoIterator<Item = (&'a str, &'a [f32])>,
    ) -> Result<(), VectorError> {
        let mut inner = self.inner.write().map_err(|_| VectorError::LockPoisoned)?;
        inner.hnsw = Self::new_hnsw(&self.cfg);
        inner.id_to_event.clear();
        inner.event_to_id.clear();
        let mut count = 0usize;
        for (event_id, vector) in iter {
            if vector.len() != self.cfg.dimensions {
                tracing::warn!(
                    event_id = %event_id,
                    expected = self.cfg.dimensions,
                    got = vector.len(),
                    "skipping vector with wrong dimensions during rebuild"
                );
                continue;
            }
            let id = inner.id_to_event.len();
            inner.hnsw.insert((vector, id));
            inner.id_to_event.push(event_id.to_string());
            inner.event_to_id.insert(event_id.to_string(), id);
            count += 1;
            if count.is_multiple_of(10_000) {
                tracing::info!(rebuilt = count, "vector index rebuild progress");
            }
        }
        inner.inserts_since_flush = 0;
        tracing::info!(elements = count, "vector index rebuild complete");
        Ok(())
    }

    /// Persist the in-memory index to disk. Idempotent. No-op for
    /// in-memory-only indexes.
    pub fn flush(&self) -> Result<(), VectorError> {
        if self.storage.is_none() {
            return Ok(());
        }
        let mut inner = self.inner.write().map_err(|_| VectorError::LockPoisoned)?;
        let res = self.dump_locked(&mut inner);
        if res.is_ok() {
            inner.inserts_since_flush = 0;
        }
        res
    }

    fn dump_locked(&self, inner: &mut Inner) -> Result<(), VectorError> {
        let storage = match &self.storage {
            Some(s) => s,
            None => return Ok(()),
        };
        // Make sure the directory exists — important when the SQLite
        // file lives in a fresh tempdir.
        if !storage.dir.exists() {
            fs::create_dir_all(&storage.dir)
                .map_err(|e| VectorError::Io(format!("create dump dir: {e}")))?;
        }
        // hnsw_rs::file_dump generates a new unique basename if the
        // graph file already exists AND mmap is on. We don't use
        // mmap, so it overwrites in place; but to be defensive
        // (some hnsw_rs versions changed that behavior) we delete
        // the prior graph + data files first.
        let _ = fs::remove_file(storage.graph_path());
        let _ = fs::remove_file(storage.data_path());
        inner
            .hnsw
            .file_dump(&storage.dir, &storage.basename)
            .map_err(|e| VectorError::Backend(format!("file_dump: {e}")))?;

        // Sidecar map file.
        let map_bytes = serde_json::to_vec(&inner.id_to_event)
            .map_err(|e| VectorError::Io(format!("map encode: {e}")))?;
        atomic_write(&storage.map_path(), &map_bytes)?;

        // Sidecar meta file. Written *last* so a torn dump (graph or
        // map missing) is detected as "missing meta" and forces a
        // rebuild rather than serving stale data.
        let meta = IndexMeta {
            version: META_VERSION,
            dimensions: self.cfg.dimensions,
            element_count: inner.id_to_event.len(),
        };
        let meta_json =
            serde_json::to_vec(&meta).map_err(|e| VectorError::Io(format!("meta encode: {e}")))?;
        let mut buf = Vec::with_capacity(META_MAGIC.len() + 1 + meta_json.len());
        buf.extend_from_slice(META_MAGIC);
        buf.push(META_VERSION);
        buf.extend_from_slice(&meta_json);
        atomic_write(&storage.meta_path(), &buf)?;
        Ok(())
    }
}

/// Write a file atomically (write to tmp, fsync, rename). Avoids
/// torn writes if the process dies mid-flush.
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), VectorError> {
    let parent = path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| VectorError::Io("bad path".to_string()))?;
    let tmp = parent.join(format!("{file_name}.tmp"));
    {
        let mut f =
            fs::File::create(&tmp).map_err(|e| VectorError::Io(format!("create tmp: {e}")))?;
        f.write_all(bytes)
            .map_err(|e| VectorError::Io(format!("write tmp: {e}")))?;
        f.sync_all()
            .map_err(|e| VectorError::Io(format!("fsync tmp: {e}")))?;
    }
    fs::rename(&tmp, path).map_err(|e| VectorError::Io(format!("rename tmp: {e}")))?;
    Ok(())
}

/// Whether the index was loaded from disk or needs rebuilding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenStatus {
    /// We successfully loaded a graph from disk.
    LoadedFromDisk,
    /// Disk artifacts missing or corrupt; caller must rebuild.
    RebuildRequired,
}

/// Convenience alias for the `Arc` we hand around.
pub type SharedVectorIndex = Arc<VectorIndex>;

/// Compute cosine *distance* between two equal-length vectors —
/// matches `hnsw_rs::dist::DistCosine`. Exposed so callers (tests,
/// the hybrid-search path) can score outside the index.
pub fn cosine_distance(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || b.is_empty() || a.len() != b.len() {
        return 1.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return 1.0;
    }
    1.0 - (dot / (na.sqrt() * nb.sqrt()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(dims: usize) -> VectorIndexConfig {
        VectorIndexConfig {
            dimensions: dims,
            flush_every_n_inserts: 10_000, // disable in unit tests
            max_elements: 1024,
            max_nb_layers: 8,
        }
    }

    #[test]
    fn upsert_and_len() {
        let idx = VectorIndex::open_in_memory(cfg(3));
        assert!(idx.is_empty());
        idx.upsert("e1", &[1.0, 0.0, 0.0]).unwrap();
        assert_eq!(idx.len(), 1);
    }

    #[test]
    fn upsert_wrong_dimensions_errs() {
        let idx = VectorIndex::open_in_memory(cfg(3));
        let err = idx.upsert("e1", &[1.0, 0.0]).unwrap_err();
        assert!(matches!(err, VectorError::DimensionMismatch { .. }));
    }

    #[test]
    fn search_empty_returns_empty() {
        let idx = VectorIndex::open_in_memory(cfg(3));
        let r = idx.search(&[1.0, 0.0, 0.0], 5).unwrap();
        assert!(r.is_empty());
    }

    #[test]
    fn search_returns_nearest() {
        // Use angularly-distinct (non-zero) vectors. Cosine distance
        // is undefined for the zero vector, so we keep all magnitudes
        // strictly positive — the test points span an arc on the
        // unit circle in the (x, y) plane.
        let idx = VectorIndex::open_in_memory(cfg(4));
        let n = 10;
        for i in 0..n {
            let theta = (i as f32) * std::f32::consts::FRAC_PI_2 / (n as f32 - 1.0);
            let v = vec![theta.cos(), theta.sin(), 0.0, 0.0];
            idx.upsert(&format!("e{i}"), &v).unwrap();
        }
        // Query near the i=3 angle.
        let theta = 3.0 * std::f32::consts::FRAC_PI_2 / (n as f32 - 1.0);
        let q = vec![theta.cos(), theta.sin(), 0.0, 0.0];
        let r = idx.search(&q, 3).unwrap();
        assert_eq!(r.len(), 3);
        assert_eq!(r[0].0, "e3");
        for w in r.windows(2) {
            assert!(w[0].1 <= w[1].1);
        }
    }

    #[test]
    fn rebuild_replaces_state() {
        let idx = VectorIndex::open_in_memory(cfg(3));
        idx.upsert("old", &[1.0, 0.0, 0.0]).unwrap();
        let v = [[0.5, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let pairs: Vec<(&str, &[f32])> = vec![("a", &v[0]), ("b", &v[1])];
        idx.rebuild_from(pairs).unwrap();
        assert_eq!(idx.len(), 2);
        // Query along (0.5, 1.0, 0.0) direction — closest to 'a'.
        let r = idx.search(&[0.5, 1.0, 0.0], 1).unwrap();
        assert_eq!(r[0].0, "a");
        // 'old' should be gone.
        assert!(!r.iter().any(|(id, _)| id == "old"));
    }

    #[test]
    fn flush_is_noop_for_in_memory_index() {
        let idx = VectorIndex::open_in_memory(cfg(3));
        idx.upsert("e", &[1.0, 0.0, 0.0]).unwrap();
        idx.flush().unwrap();
    }

    #[test]
    fn cosine_distance_known_values() {
        let d = cosine_distance(&[1.0, 0.0], &[1.0, 0.0]);
        assert!(d.abs() < 1e-6);
        let d = cosine_distance(&[1.0, 0.0], &[0.0, 1.0]);
        assert!((d - 1.0).abs() < 1e-6);
    }
}
