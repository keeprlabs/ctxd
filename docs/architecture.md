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
        SDK["First-party SDKs<br/>Rust · Python · TS"]
    end

    subgraph "ctxd daemon"
        MCP["MCP Server<br/>(rmcp; stdio + SSE + streamable-HTTP)<br/><br/>8 tools:<br/>ctx_write · ctx_read · ctx_subjects<br/>ctx_search · ctx_subscribe<br/>ctx_entities · ctx_related · ctx_timeline"]
        WIRE["Wire Protocol<br/>(MsgPack/TCP, :7778)<br/><br/>verbs:<br/>PUB · SUB · QUERY · GRANT · REVOKE · PING<br/>PEER_HELLO · PEER_WELCOME<br/>PEER_REPLICATE · PEER_ACK<br/>PEER_CURSOR_REQUEST · PEER_CURSOR<br/>PEER_FETCH_EVENTS"]
        HTTP["HTTP Server<br/>(axum, :7777)<br/><br/>endpoints:<br/>GET /health · POST /v1/grant<br/>GET /v1/stats<br/>GET /v1/peers · DELETE /v1/peers/:id<br/>GET /v1/approvals<br/>POST /v1/approvals/:id/decide"]

        CAP["Capability Engine<br/>(biscuit-auth)<br/><br/>mint · verify · attenuate · third-party blocks<br/>Stateful caveats: BudgetLimit · HumanApproval · RateLimited"]

        STORE["Store trait<br/>(ctxd-store-core)<br/><br/>SQLite (default) · Postgres · DuckDB+S3<br/>append · read · read_at · search<br/>subjects · kv_get · graph · timeline"]

        KV["KV View<br/>latest value<br/>per subject"]
        FTS["FTS View<br/>FTS5 · tsvector<br/>full-text search"]
        VEC["Vector View<br/>HNSW (hnsw_rs)<br/>persisted sidecar"]
        GRAPH["Graph View<br/>entities + relationships<br/>derived from events"]
    end

    PEER["Peer ctxd<br/>federation"]
    API["docs/api/<br/>OpenAPI · JSON Schema · msgpack hex"]

    CD & CU & CA -->|MCP| MCP
    CLI -->|direct| STORE
    ADM -->|HTTP| HTTP
    CA & SDK -->|TCP wire| WIRE
    SDK -->|HTTP| HTTP

    PEER <-->|replicate| WIRE

    MCP & WIRE & HTTP --> CAP
    CAP --> STORE
    STORE --> KV & FTS & VEC & GRAPH

    SDK -. "pinned to" .-> API
    HTTP & WIRE -. "validates against" .-> API
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
| **KV** | Latest event data per subject | "Current state of customer cust-42?" | SQLite table / Postgres row, UPSERT on append; LWW on `(time, id)` |
| **FTS** | Full-text index over event data | "Find everything mentioning 'enterprise plan'" | SQLite FTS5 virtual table; Postgres `tsvector` generated column + GIN |
| **Vector** | HNSW nearest-neighbor index | "10 most semantically similar events" | `hnsw_rs` 0.3 with on-disk sidecars (`<db>.hnsw.{graph,data,meta,map}`); rebuilt from `vector_embeddings` on corruption |
| **Graph** | Entities + relationships extracted from event payloads | "All events related to entity cust-42" | SQLite `graph_entities` + `graph_relationships` (mirrored in Postgres) |
| **Temporal** | Point-in-time event reconstruction | "What did `/work/acme/cust-42` look like on 2025-01-15?" | derived view over the events table by `time` predicate |

ctxd does NOT generate embeddings — the `Embedder` trait wraps OpenAI / Ollama / Null providers and the daemon stores whatever vector the embedder returns. Hybrid search (FTS + vector + Reciprocal Rank Fusion at `k=60`) is the default mode when an embedder is configured. See [embeddings.md](embeddings.md) and ADRs 014 / 015.

## Crate dependency graph

```mermaid
graph TD
    CORE["ctxd-core<br/>Event · Subject · PredecessorHash · Ed25519<br/>zero deps on storage/network/auth"]

    STC["ctxd-store-core<br/>Store trait + DTOs<br/>conformance test suite"]
    SS["ctxd-store-sqlite<br/>SQLite + FTS5 + HNSW + graph"]
    SP["ctxd-store-postgres<br/>tsvector FTS · advisory-lock TOCTOU"]
    SD["ctxd-store-duckobj<br/>Parquet + WAL + sidecar"]
    SHIM["ctxd-store<br/>back-compat shim"]

    CAP["ctxd-cap<br/>biscuit · third-party blocks<br/>BudgetLimit · HumanApproval · RateLimited"]
    EMBED["ctxd-embed<br/>Embedder trait<br/>OpenAI · Ollama · Null"]

    WIRE["ctxd-wire<br/>MessagePack request/response enums<br/>length-prefixed framing (leaf crate)"]

    ADCORE["ctxd-adapter-core<br/>Adapter + EventSink traits"]
    ADFS["ctxd-adapter-fs<br/>filesystem watcher"]
    ADGM["ctxd-adapter-gmail<br/>OAuth2 device flow + History API"]
    ADGH["ctxd-adapter-github<br/>PAT + ETag + rate limits"]

    MCP["ctxd-mcp<br/>stdio + SSE + streamable-HTTP<br/>8 tools"]
    HTTP["ctxd-http<br/>admin REST · approvals · peers"]
    CLI["ctxd-cli<br/>the ctxd binary"]

    SDKR["clients/rust/ctxd-client"]
    SDKP["clients/python/ctxd-py"]
    SDKT["clients/typescript/ctxd-client"]

    CORE --> STC
    STC --> SS & SP & SD
    SS --> SHIM
    CORE --> CAP
    CORE --> EMBED
    CORE --> ADCORE
    CORE --> WIRE

    ADCORE --> ADFS & ADGM & ADGH

    CORE & STC & CAP --> MCP
    CORE & STC & CAP --> HTTP
    CORE & WIRE --> CLI

    MCP & HTTP & SS & SP & SD & CAP & ADCORE & WIRE --> CLI

    SDKR -. "wraps" .-> HTTP & WIRE
    SDKP -. "wraps" .-> HTTP & WIRE
    SDKT -. "wraps" .-> HTTP & WIRE
```

`ctxd-wire` is a leaf crate — it depends on `ctxd-core` for the `Event` type but is not depended on by anything inside the workspace except `ctxd-cli` (the binary) and the Rust SDK. Splitting it out means downstream consumers (the three SDKs, federation, embedded servers) can take a wire-protocol dep without dragging in storage, capabilities, MCP, or the HTTP admin.

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

**Caveat types (v0.3):**

| Caveat | Purpose | State |
|--------|---------|-------|
| SubjectMatches | Glob pattern restricting which paths the token can access | static |
| OperationAllowed | Which operations: `read`, `write`, `subjects`, `search`, `admin`, `peer`, `subscribe` | static |
| ExpiresAt | Timestamp after which the token is invalid | static |
| KindAllowed | Restrict to specific event types (e.g., only `ctx.note`) | static |
| RateLimit | `ops_per_sec` cap, persisted 1-second windowed counter (ADR 011) | stateful |
| BudgetLimit | `(currency, amount_micro_units)` cumulative spend cap with per-op cost table | stateful |
| HumanApprovalRequired | Each verify for the named op blocks until a human decides | stateful |
| Third-party block | Authority-signed attenuation (e.g. `A → B → C` chain) verified via `verify_multi` | static |

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
    S->>CAP: verify_with_state(token, subject, "write", state)
    alt token invalid (static caveats fail)
        CAP-->>S: DENIED
        S-->>C: error: authorization denied
    else budget exceeded
        CAP-->>S: BudgetExceeded
        S-->>C: error: budget exceeded
    else approval required
        CAP-->>CAP: blocking approval_wait(timeout)
    else rate limited
        CAP-->>S: RateLimited
        S-->>C: error: rate limited (back off)
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
events             -- append-only event log: seq, id, source, subject, event_type, time,
                   --                       data, predecessorhash, signature, parents,
                   --                       attestation
event_parents      -- causal-DAG side table (event_id, parent_id) for parent backfill
kv_view            -- latest value per subject (subject PK, data, updated_at)
fts_view           -- FTS5 virtual table (event_id, subject, event_type, data)
graph_entities     -- materialized entities extracted from event payloads
graph_relationships -- edges between entities
revoked_tokens     -- biscuit token revocation list (token_id PK)
peers              -- federation peers (peer_id, public_key, url, scopes, …)
peer_cursors       -- last-seen cursor per peer for resume
token_budgets      -- BudgetLimit per (token_id, currency)
pending_approvals  -- HumanApprovalRequired queue (approval_id, decision, …)
rate_buckets       -- RateLimit 1-second windowed counter per token_id
vector_embeddings  -- raw vectors backing the persisted HNSW index
metadata           -- daemon config and ctxd_version stamp
```

Postgres mirrors the same logical tables with Postgres-native types (`UUID`, `JSONB`, `TIMESTAMPTZ`, `UUID[]`, `BYTEA`); see `docs/storage-postgres.md` and ADR 016 for the schema choices.

DuckDB+object-store keeps the event log as Parquet files behind an atomic `_manifest.json` and uses a SQLite sidecar for the same KV / peers / caveats / vectors / graph tables (ADR 018).

## Client SDKs

Three first-party SDKs ship at v0.3 alongside the daemon. All three pin to the same [`docs/api/`](api/) contract artifact (OpenAPI 3.1 + JSON Schema + MessagePack hex fixtures) and run the same conformance corpus, so a wire change either lands in every SDK or fails CI.

| Language | Package | Path | Source of truth |
|----------|---------|------|-----------------|
| Rust | `ctxd-client` (crates.io) | [`clients/rust/ctxd-client`](../clients/rust/ctxd-client/README.md) | yes |
| Python | `ctxd-client` on PyPI (imports as `ctxd`) | [`clients/python/ctxd-py`](../clients/python/ctxd-py/README.md) | mirrors Rust |
| TypeScript / JS | `@ctxd/client` on npm | [`clients/typescript/ctxd-client`](../clients/typescript/ctxd-client/README.md) | mirrors Rust |

The Rust SDK's API surface is the source of truth; Python and TypeScript mirror it method-for-method, with language-idiomatic naming and async ergonomics. The Rust workspace runs the same conformance harness in `crates/ctxd-wire/tests/conformance_corpus.rs` so the daemon is held to the same bar as the SDKs.

## What v0.3 does NOT include

| Feature | Target | Reason |
|---------|--------|--------|
| Full daemon over `dyn Store` for non-SQLite backends | v0.4 | Postgres + DuckDB run a minimal HTTP admin in v0.3 |
| Token-bucket rate limiting | v0.4 | v0.3 ships a hard 1-second windowed counter (ADR 011) |
| `budget_refund` for failed downstream ops | v0.4 | Reserve-then-commit semantics today (ADR 011) |
| Full TEE proof verification | v0.4 | Attestation field is canonicalized; verifier hook is optional (ADR 007) |
| pgvector / native vector indexes in Postgres | v0.4 | Brute-force cosine fallback today (ADR 016) |
| Slack, Notion, Linear, calendar adapters | v0.4 | Gmail + GitHub shipped in v0.3 |
| x402 HTTP 402 gateway integration | v0.4 | `BudgetLimit` enforces locally; HTTP-level micropayments are a separate protocol problem |
| DuckDB compaction / orphan-Parquet cleanup tool | v0.4 | `ctxd compact` is queued |
| Embedding generation | never | ctxd stores vectors, doesn't generate them |
