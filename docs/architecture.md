# Architecture

This document describes what ctxd is, how data flows through it, and how the pieces fit together. Written for engineers who want to understand, extend, or audit the system.

## What ctxd does

ctxd is a daemon that stores and serves context. Context is anything an AI agent might need to know: notes, documents, customer data, code changes, meeting summaries, file contents. It enters ctxd as events, gets indexed into materialized views, and leaves via MCP tool calls, wire protocol queries, or HTTP endpoints.

One binary. One SQLite file. No external services.

## System overview

```
┌──────────────────────────────────────────────────────────────────────┐
│                           CLIENTS                                     │
│                                                                       │
│  Claude Desktop    Cursor    Custom Agent    CLI    Admin UI           │
│       │               │          │            │        │               │
│       └──MCP──────────┘──MCP─────┘            │        │               │
│           (stdio)                              │        │               │
└───────────┼────────────────────────────────────┼────────┼───────────────┘
            │                                    │        │
            ▼                                    ▼        ▼
┌──────────────────────────────────────────────────────────────────────┐
│                          ctxd daemon                                  │
│                                                                       │
│  ┌─────────────┐  ┌──────────────┐  ┌─────────────────┐             │
│  │ MCP Server  │  │ Wire Protocol│  │   HTTP Server    │             │
│  │ (rmcp,      │  │ (MsgPack/TCP │  │   (axum,         │             │
│  │  stdio)     │  │  port 7778)  │  │    port 7777)    │             │
│  │             │  │              │  │                   │             │
│  │ 5 tools:    │  │ 6 verbs:     │  │ 3 endpoints:     │             │
│  │ ctx_write   │  │ PUB          │  │ GET  /health     │             │
│  │ ctx_read    │  │ SUB          │  │ POST /v1/grant   │             │
│  │ ctx_subjects│  │ QUERY        │  │ GET  /v1/stats   │             │
│  │ ctx_search  │  │ GRANT        │  │                   │             │
│  │ ctx_subscribe│ │ REVOKE       │  │                   │             │
│  │             │  │ PING         │  │                   │             │
│  └──────┬──────┘  └──────┬───────┘  └────────┬──────────┘             │
│         │                │                    │                        │
│         └────────────────┼────────────────────┘                        │
│                          │                                             │
│                          ▼                                             │
│                ┌──────────────────┐                                    │
│                │ Capability Engine│                                    │
│                │ (biscuit-auth)   │                                    │
│                │                  │                                    │
│                │ mint()           │                                    │
│                │ verify()         │                                    │
│                │ attenuate()      │                                    │
│                └────────┬─────────┘                                    │
│                         │                                              │
│                         ▼                                              │
│                ┌──────────────────┐                                    │
│                │   Event Store    │                                    │
│                │   (SQLite)       │                                    │
│                │                  │                                    │
│                │ append()  ───────┼──── within a single transaction:   │
│                │ read()           │     1. INSERT event                │
│                │ read_since()     │     2. UPSERT kv_view             │
│                │ search()         │     3. INSERT fts_view            │
│                │ subjects()       │                                    │
│                │ kv_get()         │                                    │
│                └────────┬─────────┘                                    │
│                         │                                              │
│         ┌───────────────┼───────────────┐                              │
│         ▼               ▼               ▼                              │
│  ┌────────────┐  ┌────────────┐  ┌────────────┐                      │
│  │  KV View   │  │  FTS View  │  │Vector View │                      │
│  │ latest val │  │SQLite FTS5 │  │HNSW index  │                      │
│  │ per subject│  │ full-text  │  │ in-memory  │                      │
│  └────────────┘  └────────────┘  └────────────┘                      │
└──────────────────────────────────────────────────────────────────────┘
```

## Data model

### Events

Everything in ctxd is an event. Events follow CloudEvents v1.0 with two extensions.

```
Event
├── specversion     "1.0" (always)
├── id              UUIDv7 (time-ordered, globally unique)
├── source          "ctxd://cli" (who produced this)
├── subject         "/work/acme/notes" (path-based address)
├── type            "ctx.note" (event kind)
├── time            RFC3339 timestamp
├── datacontenttype "application/json"
├── data            { ... } (any JSON payload)
├── predecessorhash SHA-256 of previous event's canonical form (our extension)
└── signature       Ed25519 signature (v0.2, our extension)
```

### Predecessor hash chain

```
Event A ──hash──> Event B ──hash──> Event C
(no pred)         pred=SHA256(A)    pred=SHA256(B)

If someone modifies Event A after the fact:
  SHA256(A') != SHA256(A) -> Event B's predecessorhash is invalid -> chain breaks
```

Canonical form for hashing: exclude `predecessorhash` and `signature`, sort keys alphabetically, serialize to JSON bytes, SHA-256.

Hash chains are scoped per subject. Events on `/work/acme` and `/personal/journal` have independent chains.

### Subjects

```
/                              root (parent of everything)
/work                          work namespace
/work/acme                     organization
/work/acme/customers/cust-42   specific entity
/personal/journal/2025-01-15   personal entry

Recursive read:
  read("/work/acme", recursive=true)
  matches: /work/acme, /work/acme/customers/cust-42, /work/acme/notes/standup
  does NOT match: /work/other, /working

Glob patterns (for capabilities):
  /**           everything
  /work/**      /work and all descendants
  /work/*       direct children of /work only (not grandchildren)
```

### Materialized views

```
Event Log (source of truth, append-only)
    |
    |-- KV View
    |   One row per subject. Stores the data from the latest event.
    |   Use case: "What is the current state of customer cust-42?"
    |   Implementation: SQLite table, UPSERT on every append.
    |
    |-- FTS View
    |   Full-text search index over event data and metadata.
    |   Use case: "Find everything mentioning 'enterprise plan'"
    |   Implementation: SQLite FTS5 virtual table.
    |
    +-- Vector View
        HNSW nearest-neighbor index over user-supplied embeddings.
        Use case: "Find the 10 events most semantically similar to this query"
        Implementation: instant-distance crate, in-memory, rebuilt on restart.
        ctxd does NOT generate embeddings. Users supply them.
```

## Crate dependency graph

```
ctxd-core  (Event, Subject, PredecessorHash. Zero deps on storage/network/auth.)
    |
    |-- ctxd-store  (SQLite event log + KV/FTS/vector views. core + sqlx + instant-distance)
    |
    |-- ctxd-cap  (Biscuit capability engine. core + biscuit-auth. No dep on store.)
    |
    |-- ctxd-adapter-core  (Adapter + EventSink traits. core only.)
    |       |-- ctxd-adapter-fs  (Filesystem watcher. adapter-core + notify)
    |       |-- ctxd-adapter-gmail  (stub)
    |       +-- ctxd-adapter-github  (stub)
    |
    |-- ctxd-mcp  (MCP server, 5 tools over stdio. core + store + cap + rmcp)
    |
    |-- ctxd-http  (Admin REST API, 3 routes. core + store + cap + axum)
    |
    +-- ctxd-cli  (The binary. Wires everything. all crates + clap + tracing + rmp-serde)
```

## Capability model

```
Mint: root key --> Token(subject="/**", ops=[read,write], expires=never)
                       |
Attenuate:             v
                   Token(subject="/work/**", ops=[read], expires=24h)
                       |
Attenuate:             v
                   Token(subject="/work/acme/**", ops=[read], expires=1h, kind=[ctx.note])

Each level can only narrow. Never widen.
```

Caveat types in v0.1:

| Caveat | Purpose |
|--------|---------|
| SubjectMatches | Glob pattern restricting which paths the token can access |
| OperationAllowed | Which operations: read, write, subjects, search, admin |
| ExpiresAt | Timestamp after which the token is invalid |
| KindAllowed | Restrict to specific event types (e.g., only ctx.note) |
| RateLimit | Ops/sec cap (stored in token, enforcement is v0.2) |

Verification is datalog-injection-safe. All user inputs are validated against `"`, `)`, `;`, and newline before interpolation into biscuit authorizer code.

## Write path

```
Request arrives (MCP, wire, or CLI)
    |
    v
Validate capability token --- rejected? --> return error
    |
    v
Parse subject path ----------- invalid? --> return error
    |
    v
BEGIN TRANSACTION
    |-- Fetch last event for this subject (for predecessor hash)
    |-- Compute SHA-256 predecessor hash
    |-- Generate UUIDv7 event ID
    |-- INSERT INTO events (...)
    |-- UPSERT INTO kv_view (...)
    +-- INSERT INTO fts_view (...)
COMMIT TRANSACTION
    |
    v
Broadcast to wire protocol SUB listeners
    |
    v
Return { id, subject, predecessorhash }
```

Steps inside the transaction are atomic. Crash at any point = rollback, views stay consistent.

## Wire protocol framing

```
+-------------+------------------------------+
| Length (4B)  | MessagePack payload          |
| u32 big-end | Request or Response enum     |
| max 16 MB   |                              |
+-------------+------------------------------+
```

## SQLite schema

```sql
events           -- append-only event log (seq, id, source, subject, type, time, data, predecessorhash)
kv_view          -- latest value per subject (subject PK, data, updated_at)
fts_view         -- FTS5 virtual table (event_id, subject, event_type, data)
metadata         -- daemon config (key-value, stores root capability key)
```

Indexes on events: `subject`, `time`, `event_type`.

## What v0.1 does NOT include

| Feature | Status | Reason |
|---------|--------|--------|
| Federation | v0.3 | Needs conflict resolution, peer discovery |
| Ed25519 signatures | v0.2 | Key management UX |
| Token revocation | v0.2 | Needs revocation list |
| Graph view | v0.2 | Needs LLM extraction |
| Temporal queries | v0.2 | Needs point-in-time reconstruction |
| Postgres/DuckDB | v0.2 | SQLite sufficient for single-node |
| Embedding generation | never | ctxd stores, not generates |
| EventQL parser | v0.2 | Basic LIKE filter for v0.1 |
| MCP over SSE/HTTP | v0.2 | stdio only |
