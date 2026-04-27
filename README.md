# ctxd

Context substrate for AI agents. Single binary, append-only event log, subject-based addressing, capability tokens, federated, MCP-native.

Not a vector DB. Not an agent framework. Not a knowledge graph. A substrate.

```mermaid
flowchart LR
    A1["Claude Desktop"] -->|"stdio"| MCP
    A2["Claude.ai / Cursor"] -->|"streamable HTTP"| MCP
    A3["Custom agent"] -->|"SSE"| MCP
    MCP["MCP server<br/>(8 tools)"] --> CTXD["ctxd daemon"]
    PEER["peer ctxd"] <-->|"replicate"| CTXD
    GH["GitHub adapter"] --> CTXD
    GM["Gmail adapter"] --> CTXD
    FS["fs adapter"] --> CTXD
    CTXD --> LOG["Event log<br/>(SQLite | Postgres | DuckDB+S3)"]
    LOG --- KV["KV"]
    LOG --- FTS["FTS"]
    LOG --- VEC["Vector<br/>(HNSW)"]
    LOG --- GRAPH["Graph"]
```

## What's in v0.3

- **Federation.** Two ctxd nodes peer with one command, replicate subjects bidirectionally, resume from persisted cursors after a crash, and verify biscuit third-party capability chains offline.
- **Three storage backends.** SQLite (default), Postgres (clustered FTS via tsvector), DuckDB-on-object-store (Parquet log on S3/R2/local fs + SQLite sidecar) — all behind a shared `Store` trait + conformance suite.
- **Multi-transport MCP.** stdio, SSE, and streamable-HTTP serve the same tool surface concurrently. Bearer-token auth on HTTP; tool-arg fallback for stdio.
- **Real ingestion adapters.** Gmail (OAuth2 device flow + AES-256-GCM token at rest + History API incremental sync). GitHub (PAT + ETag caching + rate-limit handling).
- **Hybrid search.** Pluggable embedder (OpenAI, Ollama, none); persisted HNSW index via `hnsw_rs`; FTS + vector + Reciprocal Rank Fusion.
- **Stateful caveats.** `BudgetLimit` (per-token spend ceiling) and `HumanApprovalRequired` (blocking approval flow via `ctxd approve` or `POST /v1/approvals/:id/decide`).
- **Causal DAG.** Events carry `parents` so concurrent writes are detected and resolved deterministically (LWW on `(time, id)`). Tamper-evident via predecessor hash chains; signed via Ed25519.

## Install and run

```bash
git clone https://github.com/keeprlabs/ctxd && cd ctxd
cargo build --release
# add --features storage-postgres,storage-duckdb-object for the heavier backends
```

## 60-second quickstart

```bash
# Start the daemon (SQLite + stdio MCP).
ctxd serve

# In another terminal: write a few events.
ctxd write --subject /work/acme/notes/standup --type ctx.note \
  --data '{"content":"Ship auth by Friday"}'
ctxd write --subject /work/acme/customers/cust-42 --type ctx.crm \
  --data '{"status":"interested","plan":"enterprise"}'

# Read back recursively.
ctxd read --subject /work/acme --recursive

# List subjects.
ctxd subjects --recursive

# Mint a capability scoped to /work/acme, read-only.
ctxd grant --subject "/work/acme/**" --operations "read,subjects"
```

### With semantic + hybrid search

```bash
export OPENAI_API_KEY=sk-...
ctxd serve --embedder openai
# ctx_search defaults to hybrid (FTS + vector + RRF) when an embedder is configured.
# See docs/embeddings.md for Ollama and other providers.
```

### With multi-transport MCP

```bash
ctxd serve \
  --mcp-stdio \
  --mcp-sse 127.0.0.1:7779 \
  --mcp-http 127.0.0.1:7780 \
  --require-auth
# All three transports serve the same tool surface. Auth via
# `Authorization: Bearer <base64-biscuit>` on HTTP; tool-arg fallback for stdio.
```

### With federation

```bash
# On node A:
ctxd peer grant --subjects "/work/shared/**" --expires 30d > cap-from-a.b64
# Hand cap-from-a.b64 to the operator of node B; they run:
ctxd peer add --url tcp://node-a:7778 --capability "$(cat cap-from-a.b64)"
# Both sides auto-exchange capabilities, replicate /work/shared/**,
# and resume from cursor after a restart.
```

See [docs/federation.md](docs/federation.md) for the full two-node tutorial.

### With Postgres or DuckDB+S3

```bash
ctxd serve --storage postgres \
  --storage-uri postgres://user:pass@host/ctxd

ctxd serve --storage duckdb-object \
  --storage-uri s3://my-bucket/ctxd-events
```

Postgres and DuckDB run a minimal HTTP admin (full daemon over `dyn Store` is a v0.4 follow-up). See [docs/storage-postgres.md](docs/storage-postgres.md) and [docs/storage-duckdb-object.md](docs/storage-duckdb-object.md).

## Connect Claude Desktop

```json
{
  "mcpServers": {
    "ctxd": {
      "command": "/path/to/ctxd",
      "args": ["serve", "--mcp-stdio"]
    }
  }
}
```

Claude gets eight tools: `ctx_write`, `ctx_read`, `ctx_subjects`, `ctx_search`, `ctx_subscribe`, `ctx_entities`, `ctx_related`, `ctx_timeline`.

## Why ctxd exists

Every AI agent starts each session with amnesia. Your context is scattered across Gmail, Slack, GitHub, Notion, and whatever you typed into the last chat window. Each tool has its own siloed view. None of them talk to each other. Your AI re-derives context from scratch every time.

ctxd fixes this. It's a single place where all your context lives, addressed by subject paths, secured by capability tokens, queryable by any agent over MCP, and replicated to peer nodes you trust. Write once, query from anywhere.

The event log is append-only. Every write is tamper-evident via predecessor hash chains and signed with Ed25519. Materialized views (KV, FTS, vector, graph, temporal) are derived from the log and can be rebuilt from it. Capability tokens are signed, attenuable, and bearer — an agent gets exactly the scope it needs, can delegate via biscuit third-party blocks, and cannot escalate.

## Architecture

See [docs/architecture.md](docs/architecture.md) for the full picture with diagrams.

ctxd is a Cargo workspace of 14 crates:

```
ctxd-core             Event struct, Subject paths, hash chains, Ed25519 signing.
ctxd-store-core       Store trait + shared DTOs + conformance test suite.
ctxd-store-sqlite     Default SQLite backend. KV / FTS5 / HNSW vector / graph views.
ctxd-store-postgres   Postgres backend. tsvector FTS, advisory-lock TOCTOU.
ctxd-store-duckobj    DuckDB-on-object-store. Parquet + WAL + SQLite sidecar.
ctxd-store            Back-compat shim re-exporting from ctxd-store-sqlite.
ctxd-cap              Biscuit capabilities. Third-party blocks + caveat enforcement.
ctxd-embed            Embedder trait + NullEmbedder + OpenAI + Ollama impls.
ctxd-mcp              MCP server. stdio + SSE + streamable-HTTP transports.
ctxd-http             Admin REST API (health, grant, stats, peers, approvals).
ctxd-cli              The `ctxd` binary. Wires everything together.
ctxd-adapter-core     Adapter trait + EventSink for ingestion.
ctxd-adapter-fs       Filesystem watcher adapter.
ctxd-adapter-gmail    Real Gmail adapter (OAuth2 device flow + History API).
ctxd-adapter-github   Real GitHub adapter (PAT + ETag + rate limits).
```

## API surfaces

### MCP (for agents)

| Tool | Description |
|------|-------------|
| `ctx_write` | Append an event |
| `ctx_read` | Read events under a subject (recursive optional) |
| `ctx_subjects` | List subject paths |
| `ctx_search` | FTS / vector / hybrid search (RRF) |
| `ctx_subscribe` | Poll events since a timestamp |
| `ctx_entities` | Query graph entities |
| `ctx_related` | Walk graph relationships from an entity |
| `ctx_timeline` | Read events as-of a historical timestamp |

### Wire protocol (MessagePack over TCP, port 7778)

| Verb | Description |
|------|-------------|
| `PUB` | Append an event |
| `SUB` | Subscribe (real-time broadcast) |
| `QUERY` | Query a materialized view |
| `GRANT` / `REVOKE` | Capability lifecycle |
| `PEER_HELLO` / `PEER_WELCOME` | Federation handshake |
| `PEER_REPLICATE` / `PEER_ACK` | Bidirectional event stream |
| `PEER_CURSOR_REQUEST` / `PEER_CURSOR` | Resume from last-seen |
| `PEER_FETCH_EVENTS` | Parent backfill on causal DAG gap |
| `PING` | Health check |

### HTTP (port 7777)

| Endpoint | Description |
|----------|-------------|
| `GET /health` | Health + version |
| `POST /v1/grant` | Mint a capability token |
| `GET /v1/stats` | Store statistics |
| `GET /v1/peers` | List federation peers (admin) |
| `DELETE /v1/peers/:peer_id` | Remove a federation peer (admin) |
| `GET /v1/approvals` | List pending HumanApproval requests |
| `POST /v1/approvals/:id/decide` | Allow / deny an approval (admin) |

## CLI reference

```
ctxd serve      Start daemon (HTTP + wire + MCP transports)
                Flags: --bind, --wire-bind, --mcp-stdio,
                       --mcp-sse <addr>, --mcp-http <addr>, --require-auth,
                       --embedder {null|openai|ollama},
                       --embedder-model, --embedder-url, --embedder-api-key,
                       --storage {sqlite|postgres|duckdb-object},
                       --storage-uri <uri>
ctxd write      Append an event (--subject, --type, --data, --sign)
ctxd read       Read events (--subject, --recursive)
ctxd query      Basic EventQL filter
ctxd subjects   List subjects (--prefix, --recursive)
ctxd grant      Mint a capability token
ctxd verify     Verify a token
ctxd revoke     Revoke a token by id
ctxd verify-signature  Verify an event's Ed25519 signature
ctxd peer       add | list | status | remove | grant
ctxd migrate    Re-canonicalize an existing DB at the v0.3 schema
ctxd approve    Decide a pending HumanApproval (--id, --decision)
ctxd connect    Connect to a remote daemon via wire protocol
```

Global flags: `--db <path>` (default `ctxd.db` for SQLite).

## Docs

| Document | What it covers |
|----------|---------------|
| [architecture.md](docs/architecture.md) | System design, data flow, crate map |
| [events.md](docs/events.md) | CloudEvents schema, canonical form, hash chain, parents |
| [subjects.md](docs/subjects.md) | Path syntax, recursive reads, glob patterns |
| [capabilities.md](docs/capabilities.md) | Biscuit tokens, third-party blocks, BudgetLimit, HumanApprovalRequired |
| [capability-tutorial.md](docs/capability-tutorial.md) | Hands-on walkthrough |
| [mcp.md](docs/mcp.md) | Tool reference + transports (stdio / SSE / HTTP) |
| [federation.md](docs/federation.md) | Two-node tutorial + handshake + cursor resume |
| [embeddings.md](docs/embeddings.md) | Embedder providers + hybrid search modes |
| [storage-postgres.md](docs/storage-postgres.md) | Postgres backend setup |
| [storage-duckdb-object.md](docs/storage-duckdb-object.md) | DuckDB+S3 backend setup |
| [adapters/gmail.md](docs/adapters/gmail.md) | Gmail adapter walkthrough |
| [adapters/github.md](docs/adapters/github.md) | GitHub adapter walkthrough |
| [adapter-guide.md](docs/adapter-guide.md) | Authoring a new adapter |
| [benchmarking.md](docs/benchmarking.md) | Methodology + comparisons |
| [benchmark-results.md](docs/benchmark-results.md) | Latest numbers (HNSW, FTS, federation throughput) |
| [decisions/](docs/decisions/) | 19 ADRs covering every meaningful design call |

## Client SDKs

| Language | Crate / package | Status |
|----------|-----------------|--------|
| Rust | [`ctxd-client`](clients/rust/ctxd-client/README.md) | v0.3 — published |
| Python | [`ctxd-client`](clients/python/ctxd-py/README.md) (imports as `ctxd`) | v0.3 — published |
| TypeScript | `@keeprlabs/ctxd` (npm) | v0.3 — coming next |

The Rust SDK is the source of truth: it defines the API surface the
Python and TypeScript packages mirror. All three depend on the same
`docs/api/` contract and `docs/api/conformance/` fixtures.

## Development

```bash
cargo test --workspace                   # 364 tests (default features)
cargo test --workspace --all-features    # exercises postgres + duckdb features
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

CI runs the Postgres conformance suite against a postgres:16 service container.

## License

Apache-2.0
