# ctxd

**Context substrate for AI agents.** A single-binary daemon that gives every agent — Claude Desktop, Cursor, your own code — one place to write and read shared context, with capability tokens, federation, and a native MCP server.

[![Release](https://img.shields.io/github/v/release/keeprlabs/ctxd?style=flat-square)](https://github.com/keeprlabs/ctxd/releases)
[![CI](https://img.shields.io/github/actions/workflow/status/keeprlabs/ctxd/ci.yml?branch=main&style=flat-square&label=CI)](https://github.com/keeprlabs/ctxd/actions/workflows/ci.yml)
[![License](https://img.shields.io/github/license/keeprlabs/ctxd?style=flat-square)](LICENSE)
[![Stars](https://img.shields.io/github/stars/keeprlabs/ctxd?style=flat-square)](https://github.com/keeprlabs/ctxd/stargazers)

```bash
brew install keeprlabs/tap/ctxd
ctxd serve
```

Now any MCP-aware agent — or one of the [three first-party SDKs](#build-a-client) — can write to `/work/notes/...` and read it back from anywhere else.

---

## Why ctxd

Every AI agent starts each session with amnesia. Context is scattered across Gmail, Slack, GitHub, Notion, and chat windows. None of those tools share a view, and your AI re-derives state from scratch every time.

ctxd is the place that context lives. Write once over MCP or HTTP, query from any agent, prove what was written via Ed25519 signatures, replicate to peer nodes you trust. Not a vector DB, not an agent framework, not a knowledge graph — a substrate the rest of those things plug into.

## Quickstart

```bash
# 1. Install
brew install keeprlabs/tap/ctxd

# 2. Run
ctxd serve                   # HTTP admin :7777, MCP on stdio

# 3. Use
ctxd write --subject /work/notes/standup --type ctx.note \
  --data '{"content":"Ship auth by Friday"}'
ctxd read --subject /work --recursive
```

Point Claude Desktop at it (`~/Library/Application Support/Claude/claude_desktop_config.json`):

```json
{
  "mcpServers": {
    "ctxd": { "command": "/opt/homebrew/bin/ctxd", "args": ["serve", "--mcp-stdio"] }
  }
}
```

You now have eight MCP tools wired to your context: `ctx_write`, `ctx_read`, `ctx_subjects`, `ctx_search`, `ctx_subscribe`, `ctx_entities`, `ctx_related`, `ctx_timeline`.

## How it fits

```mermaid
flowchart LR
    A1["Claude Desktop"] -->|"stdio"| MCP
    A2["Claude.ai / Cursor"] -->|"streamable HTTP"| MCP
    A3["Custom agent"] -->|"SSE"| MCP
    SDK["First-party SDKs<br/>Rust · Python · TS"] -->|"HTTP + wire"| CTXD
    MCP["MCP server<br/>(8 tools)"] --> CTXD["ctxd daemon"]
    PEER["peer ctxd"] <-->|"replicate (TCP)"| CTXD
    GH["GitHub adapter"] --> CTXD
    GM["Gmail adapter"] --> CTXD
    FS["fs adapter"] --> CTXD
    CTXD --> LOG["Event log<br/>(SQLite · Postgres · DuckDB+S3)"]
    LOG --- KV["KV"]
    LOG --- FTS["FTS"]
    LOG --- VEC["Vector<br/>(HNSW)"]
    LOG --- GRAPH["Graph"]
```

The event log is append-only. Views (KV, FTS, vector, graph, temporal) are derived from it and rebuildable from it. See [docs/architecture.md](docs/architecture.md) for the full picture.

## Features

| Feature | Description |
|---------|-------------|
| **Multi-transport** | One binary speaks HTTP admin (`:7777`), MessagePack wire (`:7778`), and MCP over stdio + SSE + streamable-HTTP — concurrently, off the same tool surface |
| **Tamper-evident log** | Append-only event log, predecessor hash chains, Ed25519 signatures, causal-DAG `parents` for deterministic conflict resolution |
| **Capability tokens** | Biscuit-based, attenuable, bearer. Stateful caveats: budget limits, human approval, rate limits |
| **Storage backends** | SQLite (default), Postgres (clustered FTS via `tsvector`), DuckDB-on-object-store (Parquet on S3 / R2 / local fs) — all behind one `Store` trait + conformance suite |
| **Federation** | Two nodes peer with one command, replicate subjects bidirectionally, resume from cursors after a crash, backfill missing parents on causal-DAG gaps |
| **Hybrid search** | Pluggable embedder (OpenAI, Ollama, none); persisted HNSW vector index + FTS fused via Reciprocal Rank Fusion |
| **Real adapters** | Gmail (OAuth2 + AES-256-GCM token at rest + History API). GitHub (PAT + ETag caching + rate limits) |
| **Three SDKs** | Rust, Python, TypeScript — all pinned to the same `docs/api/` conformance corpus the daemon runs |

## Install

### Homebrew (macOS, Linux)

```bash
brew install keeprlabs/tap/ctxd
```

### curl | sh

```bash
curl -fsSL https://github.com/keeprlabs/ctxd/releases/latest/download/install.sh | sh
```

Auto-detects OS + arch, verifies the published sha256, drops the binary in the first writable directory on `$PATH`. Override with `CTXD_INSTALL_DIR=...` (set it on the `sh` side of the pipe).

### From source

```bash
git clone https://github.com/keeprlabs/ctxd && cd ctxd
cargo build --release
# add --features storage-postgres,storage-duckdb-object for the heavier backends
```

Pre-built tarballs for macOS arm64/x86_64 and Linux x86_64/aarch64 are attached to every [release](https://github.com/keeprlabs/ctxd/releases).

## Build a client

The three first-party SDKs all wrap the same HTTP admin + wire protocol surface. Each pins to the same [`docs/api/`](docs/api/) contract.

| Language | Install | Status |
|----------|---------|--------|
| Rust | `cargo add ctxd-client` ([README](clients/rust/ctxd-client/README.md)) | v0.3 — published |
| Python | `pip install ctxd-client` (imports as `ctxd`, [README](clients/python/ctxd-py/README.md)) | v0.3 — published |
| TypeScript | `npm i @ctxd/client` ([README](clients/typescript/ctxd-client/README.md)) | v0.3 — published |

The Rust SDK is the source of truth; the Python and TypeScript packages mirror it. All three run the same MessagePack hex fixtures and JSON Schema corpus the daemon runs.

```rust
use ctxd_client::CtxdClient;
let client = CtxdClient::connect("http://127.0.0.1:7777").await?
    .with_wire("127.0.0.1:7778").await?;
let id = client.write("/work/notes", "ctx.note", json!({"hi": "there"})).await?;
```

## Going further

| Topic | Link |
|-------|------|
| Architecture, data flow, crate map | [docs/architecture.md](docs/architecture.md) |
| Events: schema, canonical form, hash chain | [docs/events.md](docs/events.md) |
| Subjects: path syntax, recursive reads | [docs/subjects.md](docs/subjects.md) |
| Capabilities: biscuit tokens, caveats | [docs/capabilities.md](docs/capabilities.md) (+ [hands-on](docs/capability-tutorial.md)) |
| MCP: tool reference + transports | [docs/mcp.md](docs/mcp.md) |
| Federation: two-node tutorial | [docs/federation.md](docs/federation.md) |
| Embeddings + hybrid search | [docs/embeddings.md](docs/embeddings.md) |
| Postgres / DuckDB+S3 backends | [storage-postgres.md](docs/storage-postgres.md) · [storage-duckdb-object.md](docs/storage-duckdb-object.md) |
| Adapters: Gmail, GitHub, authoring guide | [adapters/](docs/adapters/) · [adapter-guide.md](docs/adapter-guide.md) |
| Benchmarks (HNSW, FTS, federation) | [benchmark-results.md](docs/benchmark-results.md) |
| API contract artifact (OpenAPI + JSON Schema + msgpack hex) | [docs/api/](docs/api/) |
| Architecture decisions (19 ADRs) | [docs/decisions/](docs/decisions/) |

## Development

```bash
cargo test --workspace                       # ~425 tests (default features)
cargo test --workspace --all-features        # adds postgres + duckdb suites
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

CI runs the Postgres conformance suite against a `postgres:16` service container. The full matrix lives in [`.github/workflows/ci.yml`](.github/workflows/ci.yml).

## Contributing

Bugs, features, and adapter PRs all welcome.

- File issues at [github.com/keeprlabs/ctxd/issues](https://github.com/keeprlabs/ctxd/issues).
- For new adapters, start with [docs/adapter-guide.md](docs/adapter-guide.md) — the trait is stable.
- Open PRs against `main`. CI must be green; clippy and `cargo fmt --check` are gates.
- We aim to triage every PR within a few days.

## License

Apache-2.0
