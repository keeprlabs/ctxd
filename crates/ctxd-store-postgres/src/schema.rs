//! Schema constants and migration helpers.
//!
//! We intentionally embed the migrations as `&'static str` rather than
//! reading them from disk at runtime. The crate ships them
//! self-contained so the daemon binary doesn't have to know where the
//! migrations directory lives at install time.

use sqlx::PgPool;

use crate::store::StoreError;

/// Migration files in apply order.
///
/// Ordering matters: 0002 references nothing from 0001 directly, but
/// other backends (e.g. DuckDB-on-object-store) may add cross-table FKs
/// in later migrations and rely on this same order.
pub const MIGRATIONS: &[(&str, &str)] = &[
    ("0001_events", include_str!("../migrations/0001_events.sql")),
    ("0002_peers", include_str!("../migrations/0002_peers.sql")),
    (
        "0003_caveats",
        include_str!("../migrations/0003_caveats.sql"),
    ),
    ("0004_vector", include_str!("../migrations/0004_vector.sql")),
    ("0005_graph", include_str!("../migrations/0005_graph.sql")),
];

/// Apply every embedded migration to the supplied pool.
///
/// Each migration runs on a single dedicated connection so that
/// session-scoped state (search_path, extension visibility) is
/// consistent across all statements within a migration. Idempotency
/// is preserved at the SQL level — every `CREATE` is gated on
/// `IF NOT EXISTS`, every index is `IF NOT EXISTS`, and the pg_trgm
/// `CREATE EXTENSION` is wrapped in a `DO` block that no-ops when
/// the extension is unavailable. Calling this twice in a row is safe
/// and is exercised by the `pg_migration_idempotency` test.
pub async fn run_migrations(pool: &PgPool) -> Result<(), StoreError> {
    use sqlx::Executor;
    let mut conn = pool.acquire().await.map_err(|e| StoreError::Migration {
        name: "<acquire>".into(),
        source: e,
    })?;
    for (name, sql) in MIGRATIONS {
        tracing::debug!(migration = %name, "applying postgres migration");
        conn.execute(*sql)
            .await
            .map_err(|e| StoreError::Migration {
                name: (*name).to_string(),
                source: e,
            })?;
    }
    Ok(())
}
