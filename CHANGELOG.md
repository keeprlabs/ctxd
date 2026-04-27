# Changelog

## v0.3.1 — 2026-04-24

The launch-ready polish release. Persistent rate-limit caveat state closes the last v0.3 leftover, three first-party SDKs land alongside the daemon, and the `/v1/peers` admin surface gets a friendlier README + architecture pass.

- **First-party SDKs shipped.** Rust ([`ctxd-client`](clients/rust/ctxd-client/README.md)) on crates.io, Python ([`ctxd-client`](clients/python/ctxd-py/README.md), imports as `ctxd`) on PyPI, TypeScript ([`@ctxd/client`](clients/typescript/ctxd-client/README.md)) on npm. All three pin to the same API contract and run the same conformance corpus.
- **API contract artifact** at [`docs/api/`](docs/api/) — OpenAPI 3.1 for HTTP, JSON Schema 2020-12 for events, MessagePack hex fixtures for the wire protocol, plus a SDK<->daemon compatibility matrix. The Rust workspace runs the same conformance harness in `crates/ctxd-wire/tests/conformance_corpus.rs` so the daemon is held to the same bar as the SDKs.
- **HTTP `/v1/peers` admin endpoints.** `GET /v1/peers` lists federation peers; `DELETE /v1/peers/:peer_id` removes one. Mirrors the `ctxd peer list / remove` CLI.
- **`ctxd-wire` crate split out of `ctxd-cli`.** The MessagePack request/response enums and length-prefixed framing now live in their own leaf crate so SDKs, federation, and embedded servers can take a wire-protocol dep without dragging in storage, capabilities, MCP, or the HTTP admin.
- **Persistent rate-limit caveat state (3E).** `CaveatState::rate_check(token_id, ops_per_sec)` is now a real per-token 1-second windowed counter on all three backends. `verify_with_state` enforces it as the last gate after budget + approval. New `CapError::RateLimited { ops_per_sec }` variant. SQLite gets a `rate_buckets` table (additive, gated on `IF NOT EXISTS`); Postgres adds the same in `0003_caveats.sql`. Three integration suites pin the admit/deny boundary so a future smoother token-bucket rewrite has a regression net. ADR 011 updated.

## v0.3 — 2026-04-24

The federation, backends, and adapters release. Five phases delivered across 13 sub-agent runs and one shared review pass. **364 tests passing**, clippy clean, fmt clean. 19 ADRs cover every meaningful design call.

### Phase 1 — Foundation

- **Store trait abstraction** (`ctxd-store-core`). Shared trait + DTOs + a conformance test suite every backend runs. `ctxd-store-sqlite` is the reference impl; `ctxd-store` is a back-compat shim. ADR 017.
- **`Event.parents`** (causal DAG) and **`Event.attestation`** (TEE proof bytes) — both round-trip through canonical form, hash, and signature. Federation depends on byte-identical parents serialization across peers.
- **`ctxd migrate --to 0.3`** — re-canonicalizes existing v0.2 databases (re-computes predecessor hashes, re-signs events). Idempotent via a `metadata.ctxd_version` row.
- **MCP graph + temporal tools** wired: `ctx_entities`, `ctx_related`, `ctx_timeline`.
- v0.3 SQLite schema: `parents`, `attestation`, `event_parents`, `peers`, `peer_cursors`, `token_budgets`, `pending_approvals`, `vector_embeddings`. All migrations additive.

### Phase 2 — Federation

- **Automatic capability exchange.** `ctxd peer add --url <url>` opens a TCP handshake, mints a local cap, receives a reciprocal cap, persists pubkey + URL + granted subject globs. ADR 008.
- **Biscuit third-party blocks.** `CapEngine::attenuate_with_block` + `verify_multi`. Three-authority chain test; rejects wrong key, missing intermediate, widening, and expired chains.
- **PeerManager replication loop.** Per-peer outbound filter (subject patterns ∩ cap scope ∩ origin-peer loop guard) + inbound verify (signature + cap scope + idempotent append). New file `ctxd-cli/src/federation.rs`. ADR 009.
- **Cursor resume + parent backfill.** Receiver returns last-seen `(event_id, time)` on `PeerCursorRequest`; sender replays past it. Missing parents fetched via `PeerFetchEvents` and applied in topological order. ADR 010.
- **LWW convergence.** KV view enforces LWW on `(time, event_id)` with UUIDv7 lexicographic tiebreak — deterministic across peers. ADR 006.
- **Wire protocol** gained `PeerHello`, `PeerWelcome`, `PeerReplicate`, `PeerAck`, `PeerCursorRequest`, `PeerCursor`, `PeerFetchEvents` variants.
- **CLI**: `ctxd peer add | list | status | remove | grant`.
- 9 federation integration tests covering handshake, replication identity, three-node ring loop guard, concurrent writes, cursor resume, parent backfill, tampered events, capability violations.
- Replication throughput: **1516 events/sec** on localhost TCP. Third-party block verify: **415 µs**.

### Phase 3 — Stateful caveats + multi-transport MCP

- **`CaveatState` trait** with `InMemoryCaveatState` (fast path) and `SqliteCaveatState` (persistent). Methods: `budget_increment`, `budget_get`, `rate_check`, `approval_request`, `approval_status`, `approval_decide`, `approval_wait`.
- **`BudgetLimit(currency, amount_micro_units)` caveat**. New `OperationCost` table (read=0, write=1000, search=1000, timeline=2000 micro-units). `CapEngine::verify_with_state` enforces; old `verify` kept as a v0.2-compatible shim. ADR 011.
- **`HumanApprovalRequired(op)` caveat**. Verifier blocks up to a configurable timeout; resumes via `ctxd approve <id> --decision allow|deny` or `POST /v1/approvals/:id/decide`. Notifier broadcast channel for future adapters. Race-safe (no double-decide, no missed wakeup). ADR 012.
- **Multi-transport MCP**: stdio + SSE + streamable-HTTP serve the same `CtxdMcpServer` concurrently from one daemon. `--mcp-stdio` / `--mcp-sse <addr>` / `--mcp-http <addr>` / `--require-auth` flags. Bearer-token auth on HTTP; tool-arg fallback for stdio. Header beats arg. 1 MiB request body limit. New `http-transports` Cargo feature keeps stdio-only embedders lean. ADR 013.
- 8 MCP transport integration tests + 9 caveat integration tests.

### Phase 4 — Embeddings + real adapters + TEE

- **`ctxd-embed` crate** with `Embedder` trait. Real `OpenAiEmbedder` (feature `openai`, retry-after backoff, batch chunking at 256, key redaction in `Debug` and tracing). Real `OllamaEmbedder` (feature `ollama`).
- **HNSW vector index persisted via `hnsw_rs` 0.3**. On-disk sidecars: `<db>.hnsw.{graph,data,meta,map}`. Magic-byte precheck before `hnsw_rs::HnswIo::init` to surface corruption as a typed error rather than a panic. Crash-recovery rebuild from the `vector_embeddings` table. ADR 014.
- **Hybrid `ctx_search`**: `search_mode: fts | vector | hybrid`. Default hybrid when an embedder is configured. Reciprocal Rank Fusion (k=60). ADR 015.
- **Real Gmail adapter** (`ctxd-adapter-gmail`). OAuth2 device flow; AES-256-GCM token encryption with HKDF-SHA256 key derivation; Gmail History API incremental sync with 404 fallback to full sync; `Retry-After`-aware backoff; multi-label idempotency via SQLite cursor. 7 wiremock integration tests.
- **Real GitHub adapter** (`ctxd-adapter-github`). Fine-grained PAT auth; ETag-cached polling (issues, PRs, comments, notifications); `Link`-header pagination; `X-RateLimit-Remaining` honoring; secondary rate-limit handling. 10 wiremock integration tests.
- **TEE attestation field** rides through the canonical form unchanged. Optional `attestation_verifier` hook in `CapEngine::verify_with_state`. Full TEE SDK integration deferred to v0.4. ADR 007.
- Vector search latency at N=10k: **HNSW 601 µs** vs brute-force cosine **49.2 ms** (~82× speedup). Hybrid: **3.27 ms**.

### Phase 5 — Backends

- **`ctxd-store-postgres`**. Full conformance suite green. Postgres-idiomatic schema (`UUID`, `JSONB`, `TIMESTAMPTZ`, `UUID[]`, `BYTEA`). FTS via `tsvector` generated column + GIN index. Per-subject `pg_advisory_xact_lock` for hash-chain TOCTOU. Schema-qualified `pg_trgm` GIN for recursive reads. CI matrix entry runs against a postgres:16 service container. ADR 016.
- **`ctxd-store-duckobj`**. Append-only Parquet on object store (S3, R2, Azure, GCS, local fs via `object_store`). Atomic `_manifest.json` updates as the integrity boundary. WAL on local disk for crash safety between append and flush. SQLite sidecar holds KV / peers / caveats / vectors / graph for byte-identical LWW with the SQLite backend. 64 MB / 5 min rotation. ADR 018.
- **`--storage` CLI selector** with `storage-postgres` and `storage-duckdb-object` Cargo features. Default keeps SQLite as the always-on baseline (full daemon). Postgres + DuckDB run a minimal HTTP admin in v0.3; full daemon over `dyn Store` is queued for v0.4. ADR 019.
- 6 DuckDB-specific tests + conformance: rotation, WAL replay, manifest atomicity, concurrent appenders, recursive read, parents/attestation roundtrip.

### Other

- **CI**: GitHub Actions matrix added a `postgres` job that spins a postgres:16 service container and runs `cargo test -p ctxd-store-postgres`.
- **Benchmarks**: `docs/benchmark-results.md` updated with HNSW vs brute-force, FTS vs vector vs hybrid, and federation replication throughput.
- **Workspace tests**: 89 (v0.2 baseline) → **364**.
- **ADRs**: 006–019 (14 new in v0.3, on top of the 5 from v0.1 / v0.2).

### Deferred to v0.4 (named in ADRs, not gaps)

- Persistent rate-limit caveat state (3E). In-memory fast path is wired in `ctxd-cli/src/rate_limit.rs`.
- x402 HTTP 402 gateway integration. The `BudgetLimit` caveat enforces locally today; HTTP-level micropayments are a separate protocol problem.
- Full TEE proof verification (the field is canonicalized; verifier hook is optional).
- pgvector / native vector indexes in Postgres.
- Slack, Notion, Linear, calendar adapters.
- Full daemon (federation + MCP transports + wire) over `Arc<dyn Store>` for non-SQLite backends. Today they run a minimal HTTP admin only.
- DuckDB compaction / orphan-Parquet cleanup tool (`ctxd compact`).

## v0.2 — pre-release internal

Initial multi-crate workspace, single-instance SQLite event log, capability tokens via biscuit, MCP stdio transport, basic ingestion adapter scaffolding.

## v0.1 — internal bootstrap

Spec freeze, event envelope, subject paths, predecessor hashes.
