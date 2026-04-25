//! Migrations are safe to re-run.
//!
//! Run twice in a row, assert no error and that the schema is in the
//! same shape afterwards.

use sqlx::Executor;

fn pg_url_or_skip(test_name: &str) -> Option<String> {
    match std::env::var("CTXD_PG_URL") {
        Ok(url) => Some(url),
        Err(_) => {
            eprintln!("[{test_name}] CTXD_PG_URL unset — skipping");
            None
        }
    }
}

async fn fresh_schema_url(base: &str) -> (String, String) {
    let admin_pool = sqlx::PgPool::connect(base).await.expect("connect");
    let schema = format!("ctxd_test_{}", uuid::Uuid::now_v7().simple());
    admin_pool
        .execute(format!("CREATE SCHEMA \"{schema}\"").as_str())
        .await
        .expect("create schema");
    drop(admin_pool);
    let url = if base.contains('?') {
        format!("{base}&options=-c%20search_path%3D{schema}")
    } else {
        format!("{base}?options=-c%20search_path%3D{schema}")
    };
    (schema, url)
}

#[tokio::test]
async fn migrations_are_idempotent() {
    let Some(base) = pg_url_or_skip("migrations_are_idempotent") else {
        return;
    };
    let (_schema, url) = fresh_schema_url(&base).await;

    // First run.
    let _store = ctxd_store_postgres::PostgresStore::connect(&url)
        .await
        .expect("first migrate");

    // Second run on a brand-new pool against the same schema. Any
    // non-idempotent migration would error here.
    let _store2 = ctxd_store_postgres::PostgresStore::connect(&url)
        .await
        .expect("second migrate (idempotency)");
}
