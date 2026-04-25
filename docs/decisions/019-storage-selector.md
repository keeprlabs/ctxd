# ADR 019: Storage Selector and Minimal-Serve Fallback

## Context

Phase 5 adds two backends (`ctxd-store-postgres`, `ctxd-store-duckobj`) alongside the original `ctxd-store-sqlite`. The CLI needs a way to pick between them at startup without forcing every user to compile the heavy ones.

The complication: in v0.3, the daemon's hot paths — wire protocol server, federation `PeerManager`, MCP `CtxdMcpServer` — still hold concrete `EventStore` (the SQLite type), not `Arc<dyn Store>`. Migrating those to the trait is a larger refactor that touches every transport and would re-open conflicts already settled in Phase 2 / 3 / 4.

## Decision

1. Add `--storage {sqlite|postgres|duckdb-object}` and `--storage-uri <uri>` flags to `ctxd serve`.
2. SQLite (default) keeps the full daemon: HTTP admin + wire protocol + MCP transports + federation + caveats — exactly what shipped in Phases 1–4.
3. Postgres and DuckDB-object run a **minimal HTTP admin only** that exposes:
   - `GET /health`
   - `POST /v1/append` (JSON body `{subject, type, data}`)
   - `GET /v1/read?subject=...&recursive=...`
   This is enough to validate end-to-end correctness of the trait-based backends in production, ship metric integrations, and feed external tooling.
4. Wire / federation / MCP-over-`dyn Store` is **explicitly v0.4**. The full daemon over Postgres or DuckDB requires that refactor.

### Feature gates

`ctxd-cli/Cargo.toml`:

- `storage-postgres` — pulls in `ctxd-store-postgres` and `sqlx-postgres`.
- `storage-duckdb-object` — pulls in `ctxd-store-duckobj`, `arrow`, `parquet`, `duckdb`, `object_store`.

Default has neither — a stock `cargo install ctxd-cli` build is small (~30 MB stripped). Operators who want the heavier backends pass `--features` at install time.

## Consequences

### Pros

- All three backends share the same `Store` trait + conformance tests, which means any one of them is byte-equivalent for the data-plane operations users see.
- Operators can pick storage based on workload (laptop → sqlite, OLTP cluster → postgres, analytics → duckdb-object) without changing application code.
- The default install stays lean; heavy deps are opt-in.
- Minimal-serve mode is enough to dogfood Postgres / DuckDB in production while we land the trait-based migration.

### Cons

- A user running `--storage postgres` does **not** get federation, MCP transports, or wire protocol in v0.3. They get a smaller HTTP surface. This is a deliberate, documented tradeoff.
- Two code paths for `serve` (full vs minimal) is a temporary maintenance cost. v0.4 collapses them when the daemon hot paths take `Arc<dyn Store>` natively.

### Revisit when

- The wire protocol server, federation `PeerManager`, and MCP `CtxdMcpServer` accept `Arc<dyn Store>`. At that point the minimal-serve fallback collapses into the main flow and `--storage` becomes a one-line dispatch.
