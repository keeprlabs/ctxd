# ctxd

Context substrate for AI agents. Single binary, append-only event log, subject-based addressing, capability tokens, MCP-native.

Not a vector DB. Not an agent framework. Not a knowledge graph. A substrate.

```
Agent ----MCP----> ctxd ----SQLite----> event log
                    |                      |
                    |--- KV view ----------|  (latest value per subject)
                    |--- FTS view ---------|  (full-text search via FTS5)
                    |--- Vector view ------|  (HNSW nearest-neighbor)
```

## Install and run

```bash
git clone https://github.com/ctxd/ctxd && cd ctxd
cargo build --release
```

## 60-second quickstart

```bash
# Write three events
ctxd write --subject /work/acme/notes/standup --type ctx.note \
  --data '{"content":"Ship auth by Friday"}'

ctxd write --subject /work/acme/notes/standup --type ctx.note \
  --data '{"content":"Blocked on API review"}'

ctxd write --subject /work/acme/customers/cust-42 --type ctx.crm \
  --data '{"status":"interested","plan":"enterprise"}'

# Read back everything under /work/acme
ctxd read --subject /work/acme --recursive

# Full-text search
ctxd query 'FROM e IN events WHERE e.subject LIKE "/work/acme/%" PROJECT INTO e'

# List all subjects
ctxd subjects

# Mint a capability token scoped to /work/acme, read-only
ctxd grant --subject "/work/acme/**" --operations "read,subjects"

# Start the daemon
ctxd serve
# HTTP admin on 127.0.0.1:7777
# Wire protocol on 127.0.0.1:7778
# MCP on stdio (for Claude Desktop)
```

## Connect Claude Desktop

Add to `claude_desktop_config.json`:

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

Claude gets five tools: `ctx_write`, `ctx_read`, `ctx_subjects`, `ctx_search`, `ctx_subscribe`.

## Why ctxd exists

Every AI agent starts each session with amnesia. Your context is scattered across Gmail, Slack, GitHub, Notion, and whatever you typed into the last chat window. Each tool has its own siloed view. None of them talk to each other. Your AI re-derives context from scratch every time.

ctxd fixes this. It's a single place where all your context lives, addressed by subject paths, secured by capability tokens, queryable by any agent over MCP. Write once, query from anywhere.

The event log is append-only. Every write is tamper-evident via predecessor hash chains. Materialized views (KV, FTS, vector) are derived from the log and can be rebuilt from it. Capability tokens are signed, attenuable, and bearer. An agent gets exactly the scope it needs and cannot escalate.

## Architecture

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the full picture with diagrams.

Quick version:

```
ctxd is 10 Rust crates:

ctxd-core       Event struct, Subject paths, hash chains. Zero deps on storage/network.
ctxd-store      SQLite event log + KV/FTS/vector views. Depends only on core.
ctxd-cap        Biscuit-based capability engine. Depends only on core.
ctxd-mcp        MCP server (5 tools over stdio). Depends on core, store, cap.
ctxd-http       Admin REST API (3 endpoints). Depends on core, store, cap.
ctxd-cli        The `ctxd` binary. Wires everything together.
ctxd-adapter-core   Adapter trait + EventSink for ingestion.
ctxd-adapter-fs     Filesystem watcher adapter.
ctxd-adapter-gmail  Gmail adapter (stub).
ctxd-adapter-github GitHub adapter (stub).
```

## API surfaces

ctxd exposes three interfaces. All three read and write the same event log.

### MCP (for agents)

| Tool | Description |
|------|-------------|
| `ctx_write` | Append an event. Params: `subject`, `event_type`, `data`, `token?` |
| `ctx_read` | Read events. Params: `subject`, `recursive?`, `token?` |
| `ctx_subjects` | List subjects. Params: `prefix?`, `recursive?`, `token?` |
| `ctx_search` | Full-text search. Params: `query`, `subject_pattern?`, `k?`, `token?` |
| `ctx_subscribe` | Poll events since timestamp. Params: `subject`, `since?`, `recursive?`, `token?` |

### Wire protocol (for services, MessagePack over TCP, port 7778)

| Verb | Description |
|------|-------------|
| `PUB` | Append an event |
| `SUB` | Subscribe to a subject pattern (real-time via broadcast) |
| `QUERY` | Query a materialized view (log, kv, fts) |
| `GRANT` | Mint a capability token |
| `REVOKE` | Revoke a token (v0.2) |
| `PING` | Health check |

### HTTP (for admin, port 7777)

| Endpoint | Description |
|----------|-------------|
| `GET /health` | Health check + version |
| `POST /v1/grant` | Mint a capability token |
| `GET /v1/stats` | Store statistics |

## CLI reference

```
ctxd serve      Start daemon (HTTP :7777 + wire :7778 + MCP stdio)
ctxd write      Append an event (--subject, --type, --data)
ctxd read       Read events (--subject, --recursive)
ctxd query      EventQL query (v0.1: basic LIKE filter)
ctxd subjects   List subjects (--prefix, --recursive)
ctxd grant      Mint capability token (--subject, --operations)
ctxd verify     Verify a token (--token, --subject, --operation)
ctxd revoke     Revoke a token (v0.2 stub)
ctxd connect    Connect to remote daemon via wire protocol
```

Global flag: `--db <path>` (default: `ctxd.db`)

## Docs

| Document | What it covers |
|----------|---------------|
| [ARCHITECTURE.md](docs/ARCHITECTURE.md) | System design, data flow, crate map, diagrams |
| [events.md](docs/events.md) | CloudEvents schema, canonical form, hash chain |
| [subjects.md](docs/subjects.md) | Path syntax, recursive reads, glob patterns |
| [capabilities.md](docs/capabilities.md) | Biscuit tokens, caveats, verification model |
| [capability-tutorial.md](docs/capability-tutorial.md) | Hands-on walkthrough with CLI commands |
| [mcp.md](docs/mcp.md) | MCP tool reference with parameter tables |
| [adapter-guide.md](docs/adapter-guide.md) | How to write ingestion adapters |
| [benchmarking.md](docs/benchmarking.md) | How to benchmark ctxd against alternatives |
| [decisions/](docs/decisions/) | Architecture Decision Records |

## Development

```bash
cargo test                    # 49 tests
cargo clippy -- -D warnings   # lint
cargo fmt --check             # format check
```

## License

Apache-2.0
