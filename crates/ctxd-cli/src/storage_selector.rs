//! `--storage` selector — runtime backend choice for the daemon.
//!
//! The daemon ships three event-store backends as of v0.3:
//!
//! - `sqlite` (default) — `ctxd_store::EventStore` against a local SQLite
//!   file. Always-on baseline; no Cargo feature required.
//! - `postgres` — `ctxd_store_postgres::PostgresStore`, behind the
//!   `storage-postgres` feature.
//! - `duckdb-object` — `ctxd_store_duckobj::DuckObjStore`, behind the
//!   `storage-duckdb-object` feature.
//!
//! The selector returns an `Arc<dyn Store>` so HTTP / MCP / federation
//! call sites that already speak the trait can swap backends with no
//! recompile of their own. Concrete-typed call sites (`EventStore`-only
//! protocol/federation in v0.3) keep working with the default backend;
//! they will be migrated to the trait in the v0.4 federation pass.
//!
//! See ADR 019 for the full design.

use std::path::PathBuf;
use std::sync::Arc;

use ctxd_store_core::Store;

/// The chosen backend kind.
///
/// Parsed from the `--storage` CLI flag. Default = [`StorageKind::Sqlite`]
/// — the always-on baseline that needs no extra feature flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageKind {
    /// Local SQLite file (`ctxd_store::EventStore`). Always available.
    Sqlite,
    /// Postgres connection (`ctxd_store_postgres::PostgresStore`).
    /// Requires the `storage-postgres` Cargo feature.
    Postgres,
    /// Append-only Parquet on an object store
    /// (`ctxd_store_duckobj::DuckObjStore`). Requires the
    /// `storage-duckdb-object` Cargo feature.
    DuckdbObject,
}

impl StorageKind {
    /// Parse from the CLI string. Accepts the trailing-hyphen forms
    /// (`duckdb-object`) and the underscore form (`duckdb_object`)
    /// for shell-friendliness.
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "sqlite" => Ok(Self::Sqlite),
            "postgres" => Ok(Self::Postgres),
            "duckdb-object" | "duckdb_object" => Ok(Self::DuckdbObject),
            other => Err(format!(
                "unknown --storage value `{other}` (expected sqlite | postgres | duckdb-object)"
            )),
        }
    }
}

/// Resolved storage configuration.
#[derive(Debug, Clone)]
pub struct StorageSpec {
    /// Selected backend.
    pub kind: StorageKind,
    /// SQLite database file path (for `Sqlite`).
    pub sqlite_path: Option<PathBuf>,
    /// Connection or object-store URI (for `Postgres` / `DuckdbObject`).
    pub uri: Option<String>,
}

/// Build an `Arc<dyn Store>` for the requested backend.
///
/// For SQLite returns the new store directly. For non-default backends
/// the function gates on the corresponding Cargo feature; if the
/// feature is not enabled the call returns a clear `Err` so operators
/// see a one-line diagnosis instead of "no such module" at runtime.
#[tracing::instrument(skip_all, fields(kind = ?spec.kind))]
pub async fn select_store(spec: &StorageSpec) -> Result<Arc<dyn Store>, SelectError> {
    match spec.kind {
        StorageKind::Sqlite => {
            let path = spec
                .sqlite_path
                .clone()
                .ok_or_else(|| SelectError::missing_uri("sqlite", "--db <path>"))?;
            let store = ctxd_store::EventStore::open(&path)
                .await
                .map_err(|e| SelectError::Open(e.to_string()))?;
            Ok(Arc::new(store))
        }
        StorageKind::Postgres => {
            #[cfg(feature = "storage-postgres")]
            {
                let uri = spec.uri.clone().ok_or_else(|| {
                    SelectError::missing_uri("postgres", "--storage-uri postgres://...")
                })?;
                let store = ctxd_store_postgres::PostgresStore::connect(&uri)
                    .await
                    .map_err(|e| SelectError::Open(e.to_string()))?;
                Ok(Arc::new(store) as Arc<dyn Store>)
            }
            #[cfg(not(feature = "storage-postgres"))]
            Err(SelectError::FeatureDisabled {
                kind: "postgres",
                feature: "storage-postgres",
            })
        }
        StorageKind::DuckdbObject => {
            #[cfg(feature = "storage-duckdb-object")]
            {
                let uri = spec.uri.clone().ok_or_else(|| {
                    SelectError::missing_uri(
                        "duckdb-object",
                        "--storage-uri file:///path or s3://bucket/prefix",
                    )
                })?;
                let store = open_duckobj(&uri).await?;
                Ok(Arc::new(store) as Arc<dyn Store>)
            }
            #[cfg(not(feature = "storage-duckdb-object"))]
            Err(SelectError::FeatureDisabled {
                kind: "duckdb-object",
                feature: "storage-duckdb-object",
            })
        }
    }
}

#[cfg(feature = "storage-duckdb-object")]
async fn open_duckobj(uri: &str) -> Result<ctxd_store_duckobj::DuckObjStore, SelectError> {
    if let Some(rest) = uri.strip_prefix("file://") {
        let path = std::path::Path::new(rest);
        ctxd_store_duckobj::DuckObjStore::open_local(path)
            .await
            .map_err(|e| SelectError::Open(e.to_string()))
    } else if uri.starts_with("s3://") || uri.starts_with("r2://") || uri.starts_with("gs://") {
        Err(SelectError::Open(format!(
            "v0.3 ships --storage duckdb-object with local-fs and explicit object_store \
             handles only; cloud URIs ({uri}) are tracked for v0.4 — see \
             docs/storage-duckdb-object.md for the construction snippet"
        )))
    } else {
        // Treat unprefixed paths as local-fs.
        let path = std::path::Path::new(uri);
        ctxd_store_duckobj::DuckObjStore::open_local(path)
            .await
            .map_err(|e| SelectError::Open(e.to_string()))
    }
}

/// Errors from [`select_store`].
#[derive(Debug, thiserror::Error)]
pub enum SelectError {
    /// The requested backend was not compiled in.
    #[error(
        "--storage {kind} requires the `{feature}` Cargo feature; \
         rebuild ctxd with `--features {feature}`"
    )]
    FeatureDisabled {
        /// Backend kind requested.
        kind: &'static str,
        /// Feature that would unlock it.
        feature: &'static str,
    },

    /// The caller didn't pass the URI / path the backend needs.
    #[error("--storage {kind} needs {flag}")]
    MissingUri {
        /// Backend kind requested.
        kind: &'static str,
        /// Suggestion (e.g. `--storage-uri postgres://...`).
        flag: &'static str,
    },

    /// The backend rejected the open call (bad credentials, unreachable host, ...).
    #[error("backend open failed: {0}")]
    Open(String),
}

impl SelectError {
    fn missing_uri(kind: &'static str, flag: &'static str) -> Self {
        Self::MissingUri { kind, flag }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_parses_canonical_forms() {
        assert_eq!(StorageKind::parse("sqlite").unwrap(), StorageKind::Sqlite);
        assert_eq!(
            StorageKind::parse("postgres").unwrap(),
            StorageKind::Postgres
        );
        assert_eq!(
            StorageKind::parse("duckdb-object").unwrap(),
            StorageKind::DuckdbObject
        );
        assert_eq!(
            StorageKind::parse("duckdb_object").unwrap(),
            StorageKind::DuckdbObject
        );
        assert!(StorageKind::parse("nope").is_err());
    }
}
