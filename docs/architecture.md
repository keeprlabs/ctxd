# Architecture

ctxd is a single-binary daemon that ingests, stores, addresses, and serves context to AI agents.

## Core Principles

1. **Event log is the source of truth.** Append-only. All materialized views (KV, FTS, vector) are derived from the log and can be rebuilt.
2. **CloudEvents v1.0 spec.** Every event follows the standard with our extensions (`predecessorhash`, `signature`).
3. **Subjects are paths.** `/work/acme/customers/cust-42`, not dotted. Recursive reads, glob wildcards.
4. **Predecessor hash chains.** SHA-256 hash of the previous event's canonical form per subject tree. Tamper-evidence without consensus.
5. **Capabilities, not ACLs.** Biscuit tokens: signed, attenuable, bearer. Grant/verify/attenuate in v0.1; revocation in v0.2.

## Component Diagram

```
┌─────────────────────────────────────────────────────────┐
│                      ctxd daemon                         │
│                                                          │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐              │
│  │ MCP      │  │ HTTP     │  │ CLI      │              │
│  │ (stdio)  │  │ (axum)   │  │ (clap)   │              │
│  └────┬─────┘  └────┬─────┘  └────┬─────┘              │
│       │              │              │                    │
│       └──────────────┼──────────────┘                    │
│                      │                                   │
│              ┌───────┴───────┐                           │
│              │  Capability   │                           │
│              │  Engine       │                           │
│              │  (biscuit)    │                           │
│              └───────┬───────┘                           │
│                      │                                   │
│              ┌───────┴───────┐                           │
│              │  Event Store  │                           │
│              │  (SQLite)     │                           │
│              └───────┬───────┘                           │
│                      │                                   │
│       ┌──────────────┼──────────────┐                    │
│       │              │              │                    │
│  ┌────┴────┐   ┌────┴────┐   ┌────┴────┐              │
│  │ KV View │   │FTS View │   │Vec View │              │
│  │ (latest │   │ (FTS5)  │   │ (stub)  │              │
│  │  value) │   │         │   │         │              │
│  └─────────┘   └─────────┘   └─────────┘              │
└─────────────────────────────────────────────────────────┘
```

## Crate Dependencies

```
ctxd-core (zero external deps besides serde/sha2/uuid/chrono)
    │
    ├── ctxd-store (core + sqlx)
    ├── ctxd-cap (core + biscuit-auth)
    │
    ├── ctxd-mcp (core + store + cap + rmcp)
    ├── ctxd-http (core + store + cap + axum)
    │
    └── ctxd-cli (all of the above + clap + tracing)
```

## Data Flow

1. Agent sends `ctx_write` via MCP (or CLI runs `ctxd write`)
2. Capability token is verified (if provided)
3. Subject path is validated
4. Predecessor hash is computed from the last event for that subject
5. Event is appended to the SQLite event log
6. KV view is updated (upsert latest value per subject)
7. FTS view is updated (indexed for full-text search)
8. Response returned with event ID and predecessor hash
