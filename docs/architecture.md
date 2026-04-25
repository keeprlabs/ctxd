# Architecture

How ctxd works, how data flows through it, and how the pieces fit together. Written for engineers who want to understand, extend, or audit the system.

## What ctxd does

ctxd is a daemon that stores and serves context for AI agents. Context is anything an agent might need: notes, documents, customer data, code changes, meeting summaries, file contents. It enters as events, gets indexed into materialized views, and leaves via MCP tool calls, wire protocol queries, or HTTP endpoints.

One binary. SQLite by default — Postgres available for operators who want a managed datastore. No external services required.

Storage backends live behind the [`Store`](../crates/ctxd-store-core/src/lib.rs) trait (v0.3+); see ADR 017 for the conformance pattern that gates every backend, ADR 016 for the Postgres schema choices, and `docs/storage-postgres.md` for operator setup.

## System overview

```mermaid
flowchart TB
    subgraph Clients
        CD[Claude Desktop]
        CU[Cursor]
        CA[Custom Agent]
        CLI[CLI]
        ADM[Admin UI]
    end

    subgraph "ctxd daemon"
        MCP["MCP Server\n(rmcp, stdio)\n\n5 tools:\nctx_write\nctx_read\nctx_subjects\nctx_search\nctx_subscribe"]
        WIRE["Wire Protocol\n(MsgPack/TCP, :7778)\n\n6 verbs:\nPUB, SUB, QUERY\nGRANT, REVOKE, PING"]
        HTTP["HTTP Server\n(axum, :7777)\n\n3 endpoints:\nGET /health\nPOST /v1/grant\nGET /v1/stats"]

        CAP["Capability Engine\n(biscuit-auth)\n\nmint / verify / attenuate"]

        STORE["Event Store\n(SQLite)\n\nappend / read / read_since\nsearch / subjects / kv_get"]

        KV["KV View\nlatest value\nper subject"]
        FTS["FTS View\nSQLite FTS5\nfull-text search"]
        VEC["Vector View\nHNSW index\nin-memory"]
    end

    CD & CU & CA -->|MCP stdio| MCP
    CLI -->|direct| STORE
    ADM -->|HTTP| HTTP
    CA -->|TCP| WIRE

    MCP & WIRE & HTTP --> CAP
    CAP --> STORE
    STORE --> KV & FTS & VEC
```

## Data model

### Events

Everything in ctxd is an event. Events follow CloudEvents v1.0 with two extensions.

| Field | Type | Description |
|-------|------|-------------|
| `specversion` | `"1.0"` | Always 1.0 |
| `id` | UUIDv7 | Time-ordered, globally unique |
| `source` | string | Who produced this (e.g., `"ctxd://cli"`) |
| `subject` | string | Path-based address (e.g., `"/work/acme/notes"`) |
| `type` | string | Event kind (e.g., `"ctx.note"`) |
| `time` | RFC3339 | When the event was created |
| `datacontenttype` | string | Always `"application/json"` |
| `data` | JSON | Any JSON payload |
| `predecessorhash` | string | SHA-256 of previous event's canonical form (ctxd extension) |
| `signature` | string | Ed25519 signature (v0.2, ctxd extension) |

### Predecessor hash chain

```mermaid
flowchart LR
    A["Event A\npredecessorhash = null\n(first in chain)"]
    B["Event B\npredecessorhash =\nSHA256(canonical(A))"]
    C["Event C\npredecessorhash =\nSHA256(canonical(B))"]

    A -->|"SHA-256"| B -->|"SHA-256"| C
```

Canonical form for hashing: exclude `predecessorhash` and `signature`, sort keys alphabetically, serialize to JSON bytes, SHA-256.

Hash chains are scoped per subject. Events on `/work/acme` and `/personal/journal` have independent chains. If any event is modified after the fact, the next event's predecessor hash will not match, and the chain breaks.

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

All views are derived from the append-only event log and can be rebuilt from it.

| View | What it stores | Use case | Implementation |
|------|---------------|----------|----------------|
| **KV** | Latest event data per subject | "Current state of customer cust-42?" | SQLite table, UPSERT on append |
| **FTS** | Full-text index over event data | "Find everything mentioning 'enterprise plan'" | SQLite FTS5 virtual table |
| **Vector** | HNSW nearest-neighbor index | "10 most semantically similar events" | instant-distance crate, in-memory, rebuilt on restart |

ctxd does NOT generate embeddings. Users supply them.

## Crate dependency graph

```mermaid
graph TD
    CORE["ctxd-core\nEvent, Subject, PredecessorHash\nZero deps on storage/network/auth"]

    STORE["ctxd-store\nSQLite event log\nKV / FTS / Vector views"]
    CAP["ctxd-cap\nBiscuit capability engine"]
    ADCORE["ctxd-adapter-core\nAdapter + EventSink traits"]

    ADFS["ctxd-adapter-fs\nFilesystem watcher"]
    ADGM["ctxd-adapter-gmail\n(stub)"]
    ADGH["ctxd-adapter-github\n(stub)"]

    MCP["ctxd-mcp\nMCP server, 5 tools\nover stdio"]
    HTTP["ctxd-http\nAdmin REST API\n3 routes"]

    CLI["ctxd-cli\nThe binary\nWires everything together"]

    CORE --> STORE
    CORE --> CAP
    CORE --> ADCORE

    ADCORE --> ADFS
    ADCORE --> ADGM
    ADCORE --> ADGH

    CORE & STORE & CAP --> MCP
    CORE & STORE & CAP --> HTTP

    MCP & HTTP & STORE & CAP & ADCORE --> CLI
```

## Capability model

```mermaid
flowchart TD
    ROOT["Root Key"]
    T1["Token A\nsubject = /**\nops = read, write\nexpires = never"]
    T2["Token B\nsubject = /work/**\nops = read\nexpires = 24h"]
    T3["Token C\nsubject = /work/acme/**\nops = read\nexpires = 1h\nkind = ctx.note"]

    ROOT -->|mint| T1
    T1 -->|attenuate| T2
    T2 -->|attenuate| T3

    style T1 fill:#f9f9f9,stroke:#333,stroke-width:2px
    style T2 fill:#f0f0f0,stroke:#333,stroke-width:2px
    style T3 fill:#e7e7e7,stroke:#333,stroke-width:2px
```

Each level can only narrow scope. Never widen.

**Caveat types in v0.1:**

| Caveat | Purpose |
|--------|---------|
| SubjectMatches | Glob pattern restricting which paths the token can access |
| OperationAllowed | Which operations: read, write, subjects, search, admin |
| ExpiresAt | Timestamp after which the token is invalid |
| KindAllowed | Restrict to specific event types (e.g., only `ctx.note`) |
| RateLimit | Ops/sec cap (stored in token, enforcement is v0.2) |

Verification is datalog-injection-safe. All user inputs are validated against `"`, `)`, `;`, and newline before interpolation into biscuit authorizer code.

## Write path

```mermaid
sequenceDiagram
    participant C as Client
    participant S as Server<br/>(MCP / Wire / CLI)
    participant CAP as Capability Engine
    participant DB as Event Store<br/>(SQLite)
    participant SUB as SUB listeners

    C->>S: write(subject, type, data, token?)
    S->>CAP: verify(token, subject, "write")
    alt token invalid
        CAP-->>S: DENIED
        S-->>C: error: authorization denied
    end
    CAP-->>S: OK

    S->>DB: BEGIN TRANSACTION
    DB->>DB: Fetch last event for subject
    DB->>DB: Compute SHA-256 predecessor hash
    DB->>DB: Generate UUIDv7 event ID
    DB->>DB: INSERT INTO events
    DB->>DB: UPSERT INTO kv_view
    DB->>DB: INSERT INTO fts_view
    S->>DB: COMMIT

    DB-->>S: {id, subject, predecessorhash}
    S->>SUB: broadcast event
    S-->>C: {id, subject, predecessorhash}
```

All steps inside the transaction are atomic. Crash at any point = rollback, views stay consistent with the log.

## Read path

```mermaid
sequenceDiagram
    participant C as Client
    participant S as Server<br/>(MCP / Wire / CLI)
    participant CAP as Capability Engine
    participant DB as Event Store<br/>(SQLite)

    C->>S: read(subject, recursive?, token?)
    S->>CAP: verify(token, subject, "read")
    alt token invalid
        CAP-->>S: DENIED
        S-->>C: error: authorization denied
    end
    CAP-->>S: OK

    alt recursive = true
        S->>DB: SELECT * FROM events<br/>WHERE subject LIKE '/prefix/%'<br/>ORDER BY seq
    else exact match
        S->>DB: SELECT * FROM events<br/>WHERE subject = '/exact'<br/>ORDER BY seq
    end

    DB-->>S: event rows
    S-->>C: JSON array of events
```

## Wire protocol framing

```mermaid
flowchart LR
    LEN["Length\n4 bytes\nu32 big-endian\nmax 16 MB"]
    PAY["MessagePack Payload\nRequest or Response enum"]

    LEN --> PAY

    style LEN fill:#333,color:#fff,stroke:#333
    style PAY fill:#f9f9f9,stroke:#333,stroke-width:2px
```

Every message on the TCP wire is length-prefixed. The length field is a 4-byte big-endian unsigned integer. The payload is a MessagePack-encoded request or response enum. Maximum payload size is 16 MB.

## SQLite schema

```sql
events           -- append-only event log (seq, id, source, subject, type, time, data, predecessorhash)
kv_view          -- latest value per subject (subject PK, data, updated_at)
fts_view         -- FTS5 virtual table (event_id, subject, event_type, data)
metadata         -- daemon config (key-value, stores root capability key)
```

Indexes on events: `subject`, `time`, `event_type`.

## What v0.1 does NOT include

| Feature | Target | Reason |
|---------|--------|--------|
| Federation | v0.3 | Needs conflict resolution, peer discovery |
| Ed25519 signatures | v0.2 | Key management UX |
| Token revocation | v0.2 | Needs revocation list |
| Graph view | v0.2 | Needs LLM extraction |
| Temporal queries | v0.2 | Needs point-in-time reconstruction |
| Postgres backend | v0.3 | Shipped — `ctxd-store-postgres` (ADR 016) |
| DuckDB+object-store backend | v0.3 (in flight) | Phase 5B parallel agent |
| Embedding generation | never | ctxd stores, not generates |
| EventQL parser | v0.2 | Basic LIKE filter for v0.1 |
| MCP over SSE/HTTP | v0.2 | stdio only |
