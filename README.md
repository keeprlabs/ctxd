<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/img/logo-dark.svg">
    <img alt="ctxd" src="assets/img/logo-light.svg" width="96" height="96">
  </picture>
</p>

<h1 align="center">Every AI on your machine, <em>one memory.</em></h1>

<p align="center">
  <strong>Star us&nbsp;❤️&nbsp;→</strong>&nbsp;<a href="https://github.com/keeprlabs/ctxd" title="Star ctxd on GitHub — click then use the ⭐ button at the top of the repo page"><img alt="Star ctxd on GitHub" src="https://img.shields.io/github/stars/keeprlabs/ctxd?style=social&label=Star"></a>
  &nbsp;·&nbsp;
  <a href="https://keeprlabs.github.io/ctxd/">🌐&nbsp;ctxd</a>
  &nbsp;·&nbsp;
  <a href="docs/onboarding.md">📖&nbsp;Docs</a>
  &nbsp;·&nbsp;
  <a href="https://github.com/keeprlabs/ctxd/releases">📦&nbsp;Releases</a>
</p>

<p align="center">
  <a href="https://github.com/keeprlabs/ctxd/releases"><img alt="Release" src="https://img.shields.io/github/v/release/keeprlabs/ctxd?style=flat-square&color=24292e"></a>
  <a href="https://github.com/keeprlabs/ctxd/actions/workflows/ci.yml"><img alt="CI" src="https://img.shields.io/github/actions/workflow/status/keeprlabs/ctxd/ci.yml?branch=main&style=flat-square&label=CI"></a>
  <a href="LICENSE"><img alt="License" src="https://img.shields.io/github/license/keeprlabs/ctxd?style=flat-square&color=24292e"></a>
</p>

<!--
  crates.io / PyPI / npm badges live in clients/<lang>/README.md.
  They're omitted from the top-level README until each registry
  actually has a v0.4 release for `ctxd-cli`, `ctxd-client`, and
  `@ctxd/client` — shields.io would otherwise render "not found"
  badges for unpublished versions, and a half-published row reads
  worse than no row at all.
-->


<p align="center">
  <strong>ctxd</strong> is a single-binary daemon that gives every MCP-aware AI tool on your machine — Claude Desktop, Claude Code, Codex — <em>one shared memory.</em> Append-only event log, capability tokens, federation, embedded dashboard. <strong>One command sets it up.</strong>
</p>

```bash
brew install keeprlabs/tap/ctxd
ctxd onboard
```

That's it. `ctxd onboard` installs ctxd as a background service, configures Claude Desktop / Claude Code / Codex over MCP, mints scoped capability tokens per app, and seeds a baseline `/me/**` so a fresh AI conversation starts with non-empty context. Two minutes, idempotent, fully reversible with `ctxd offboard`.

<p align="center">
  <img alt="ctxd onboard demo: snapshot, configure clients, mint caps, seed /me/**" src="assets/img/terminal.gif" width="100%">
</p>

---

## The pitch in one paragraph

Every AI agent starts each session with amnesia. Context is scattered across Gmail, Slack, GitHub, Notion, and chat windows. None of those tools share a view, and your AI re-derives state from scratch every time. **ctxd is the place that context lives.** Write once over MCP or HTTP, query from any agent, prove what was written via Ed25519 signatures, replicate to peer nodes you trust. Not a vector DB, not an agent framework, not a knowledge graph — a substrate the rest of those things plug into.

## Why this is different

|   | Today | With ctxd |
|---|-------|-----------|
| **Setup** | Hand-edit JSON for each AI app, paste tokens, hope nothing drifts | `ctxd onboard` — one command, all clients wired |
| **Memory across tools** | Claude Desktop and Claude Code don't share a byte | Same SQLite log, eight MCP tools, every agent reads/writes the same store |
| **Trust** | Agents write into a shared bucket, no provenance | Ed25519-signed events, biscuit capability tokens, per-client cap files |
| **Observability** | You guess what your agent wrote | Embedded web dashboard at `127.0.0.1:7777` — live event tail, subject tree, search |
| **Backends** | Pick one and migrate later | SQLite, Postgres (clustered FTS), DuckDB-on-S3 — all behind one trait + conformance suite |

## Quickstart

```bash
# 1. Install
brew install keeprlabs/tap/ctxd

# 2. One-time setup (installs the daemon as a service,
#    configures Claude Desktop / Code / Codex)
ctxd onboard

# 3. Use any of your AI tools — they all share the same memory now.
#    Or write directly via the CLI:
ctxd write --subject /work/notes/standup --type ctx.note \
  --data '{"content":"Ship auth by Friday"}'
ctxd read --subject /work --recursive
```

`ctxd onboard` is idempotent — re-running it updates configs and re-mints caps without losing data. Use `ctxd offboard` to fully reverse the install (restore client configs from snapshot, stop the service, optionally `--purge` the DB). Full walkthrough: [docs/onboarding.md](docs/onboarding.md).

You now have eight MCP tools wired to your context: `ctx_write`, `ctx_read`, `ctx_subjects`, `ctx_search`, `ctx_subscribe`, `ctx_entities`, `ctx_related`, `ctx_timeline`.

**Want your inbox and pull requests in here too?** The Gmail and GitHub adapters are shipped as separate binaries you run alongside the daemon. See [docs/adapters/running.md](docs/adapters/running.md) for the build + launchd / systemd-user recipe.

### Foreground / advanced

If you'd rather run ctxd in a terminal tab without installing a service:

```bash
ctxd serve                   # HTTP admin :7777, MCP on stdio
```

Wire Claude Desktop / Code / Codex by hand with the snippets in [docs/onboarding.md](docs/onboarding.md#manual-client-config).

## Watch what your agents are writing

```bash
ctxd dashboard
```

Opens an embedded web UI at `http://127.0.0.1:7777/`. Watch events stream in live via SSE, browse the subject tree, search the log, see which capability wrote what. Read-only by default — writes still go through MCP, the wire protocol, or the CLI. Localhost-only with DNS-rebinding defenses (host-header check, CSP, X-Frame-Options).

<p align="center">
  <img alt="ctxd dashboard demo: stats, subject tree, live event tail" src="assets/img/dashboard.gif" width="100%">
</p>

The dashboard ships in the daemon, not as a separate process. If `ctxd serve` is already running, just point your browser at `http://127.0.0.1:7777/`. See [docs/dashboard.md](docs/dashboard.md) for the security model and what each view shows.

## Architecture

<p align="center">
  <img alt="ctxd architecture: clients reach surfaces (HTTP/wire/MCP), gated by capability tokens, persisted to an append-only event store, projected into KV/FTS/vector/graph/temporal views" src="assets/img/architecture.svg" width="100%">
</p>

The event log is append-only. Views (KV, FTS, vector, graph, temporal) are derived from it and rebuildable from it. Federation is event replay across signed cursors. Capabilities gate every write at the surface layer, before anything touches the store. See [docs/architecture.md](docs/architecture.md) for the full picture, or the [animated landing page](https://keeprlabs.github.io/ctxd/) for the diagram in motion.

## What's in the box

| Capability | Detail |
|------------|--------|
| **One-command onboarding** | `ctxd onboard` installs the service, configures clients, mints caps, seeds `/me/**`. `ctxd doctor` verifies. `ctxd offboard` restores from snapshot. |
| **MCP-native** | Eight tools (`ctx_write` / `ctx_read` / `ctx_subjects` / `ctx_search` / `ctx_subscribe` / `ctx_entities` / `ctx_related` / `ctx_timeline`) over stdio, SSE, and streamable-HTTP — concurrently, off the same surface. |
| **Embedded dashboard** | Localhost web UI: live SSE event tail, subject tree, full-text search, peer view. Read-only, DNS-rebinding-safe. |
| **Tamper-evident log** | Append-only, predecessor hash chains, Ed25519 signatures, causal-DAG `parents` for deterministic conflict resolution. |
| **Capability tokens** | Biscuit-based, attenuable, bearer. Stateful caveats: budget limits, human approval, rate limits. Per-client cap files, never in process args. |
| **Storage backends** | SQLite (default), Postgres (clustered FTS via `tsvector`), DuckDB-on-object-store (Parquet on S3 / R2 / local fs) — all behind one `Store` trait + conformance suite. |
| **Federation** | Two nodes peer with one command, replicate subjects bidirectionally, resume from cursors after a crash, backfill missing parents on causal-DAG gaps. |
| **Hybrid search** | Pluggable embedder (OpenAI, Ollama, none); persisted HNSW vector index + FTS fused via Reciprocal Rank Fusion. |
| **Real adapters** | Gmail (OAuth2 + AES-256-GCM token at rest + History API). GitHub (PAT + ETag caching + rate limits). |
| **Three SDKs** | Rust, Python, TypeScript — all pinned to the same [`docs/api/`](docs/api/) conformance corpus the daemon runs. |
| **Apache-2.0** | All of it. No open-core split. |

## Build a client

The three first-party SDKs all wrap the same HTTP admin + wire protocol surface. Each pins to the same [`docs/api/`](docs/api/) contract.

| Language | Install | README |
|----------|---------|--------|
| Rust | `cargo add ctxd-client` | [clients/rust](clients/rust/ctxd-client/README.md) |
| Python | `pip install ctxd-client` (imports as `ctxd`) | [clients/python](clients/python/ctxd-py/README.md) |
| TypeScript | `npm i @ctxd/client` | [clients/typescript](clients/typescript/ctxd-client/README.md) |

The Rust SDK is the source of truth; the Python and TypeScript packages mirror it. All three run the same MessagePack hex fixtures and JSON Schema corpus the daemon runs.

```rust
use ctxd_client::CtxdClient;
let client = CtxdClient::connect("http://127.0.0.1:7777").await?
    .with_wire("127.0.0.1:7778").await?;
let id = client.write("/work/notes", "ctx.note", json!({"hi": "there"})).await?;
```

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

## Going further

| Topic | Link |
|-------|------|
| Architecture, data flow, crate map | [docs/architecture.md](docs/architecture.md) |
| Onboarding: `ctxd onboard` deep dive | [docs/onboarding.md](docs/onboarding.md) |
| Events: schema, canonical form, hash chain | [docs/events.md](docs/events.md) |
| Subjects: path syntax, recursive reads | [docs/subjects.md](docs/subjects.md) |
| Capabilities: biscuit tokens, caveats | [docs/capabilities.md](docs/capabilities.md) (+ [hands-on](docs/capability-tutorial.md)) |
| MCP: tool reference + transports | [docs/mcp.md](docs/mcp.md) |
| Federation: two-node tutorial | [docs/federation.md](docs/federation.md) |
| Embeddings + hybrid search | [docs/embeddings.md](docs/embeddings.md) |
| Postgres / DuckDB+S3 backends | [storage-postgres.md](docs/storage-postgres.md) · [storage-duckdb-object.md](docs/storage-duckdb-object.md) |
| Adapters: Gmail, GitHub, authoring guide | [adapters/](docs/adapters/) · [running guide](docs/adapters/running.md) · [adapter-guide.md](docs/adapter-guide.md) |
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

## Star history

If ctxd is useful to you, a star is the single most useful signal you can send. It tells us this approach matters, helps other developers find the project, and shapes what we prioritize next.

<p align="center">
  <a href="https://github.com/keeprlabs/ctxd"><img alt="Star history" src="https://api.star-history.com/svg?repos=keeprlabs/ctxd&type=Date" width="80%"></a>
</p>

## License

Apache-2.0. All of it.
