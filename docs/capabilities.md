# Capabilities

ctxd uses capability-based authorization via [Biscuit tokens](https://www.biscuitsec.org/).

## Core concepts

- **Capabilities, not ACLs.** Access is granted by possessing a signed token, not by being on a list.
- **Attenuable.** A token holder can create a restricted version of their token (narrower scope, fewer operations) and pass it to someone else. They cannot widen it.
- **Bearer tokens.** Whoever holds the token can use it. Protect them like passwords.

## Operations

| Operation | Description |
|-----------|-------------|
| `read` | Read events from subjects |
| `write` | Append events to subjects |
| `subjects` | List subjects |
| `search` | Full-text search events |
| `admin` | Admin operations (mint new tokens) |

## Token attenuation

Tokens form a tree. Each child token is cryptographically bound to its parent and can only narrow scope, never widen it.

```mermaid
flowchart TD
    ROOT["Root Key\n(stored in SQLite metadata)"]

    T1["Agent Token\nsubject: /**\nops: read, write, subjects, search\nexpiry: none"]

    T2["Team Token\nsubject: /work/**\nops: read, write\nexpiry: 30d"]

    T3["Read-Only Token\nsubject: /work/acme/**\nops: read, subjects\nexpiry: 24h"]

    T4["Narrow Token\nsubject: /work/acme/notes/**\nops: read\nexpiry: 1h\nkind: ctx.note"]

    T5["Contractor Token\nsubject: /work/acme/docs/**\nops: read\nexpiry: 7d"]

    ROOT -->|mint| T1
    T1 -->|attenuate| T2
    T2 -->|attenuate| T3
    T3 -->|attenuate| T4
    T2 -->|attenuate| T5

    style ROOT fill:#333,color:#fff,stroke:#333
    style T1 fill:#f9f9f9,stroke:#333,stroke-width:2px
    style T2 fill:#f0f0f0,stroke:#333,stroke-width:2px
    style T3 fill:#e7e7e7,stroke:#333,stroke-width:2px
    style T4 fill:#dedede,stroke:#333,stroke-width:2px
    style T5 fill:#dedede,stroke:#333,stroke-width:2px
```

## Verification flow

Every operation (read, write, search, etc.) passes through the capability engine before reaching the event store.

```mermaid
flowchart TD
    REQ["Incoming request\n(subject, operation, token?)"]

    CHECK{"Token\nprovided?"}

    OPEN["Allow\n(open by default in v0.1)"]

    SIG{"Signature\nvalid?"}
    DENY1["DENIED\ninvalid signature"]

    SUB{"Subject matches\ntoken glob?"}
    DENY2["DENIED\nsubject out of scope"]

    OPS{"Operation in\nallowed set?"}
    DENY3["DENIED\noperation not permitted"]

    EXP{"Token\nexpired?"}
    DENY4["DENIED\ntoken expired"]

    ALLOW["ALLOWED\nproceed to store"]

    REQ --> CHECK
    CHECK -->|no| OPEN
    CHECK -->|yes| SIG
    SIG -->|no| DENY1
    SIG -->|yes| SUB
    SUB -->|no| DENY2
    SUB -->|yes| OPS
    OPS -->|no| DENY3
    OPS -->|yes| EXP
    EXP -->|yes| DENY4
    EXP -->|no| ALLOW
```

## Minting

```bash
# Mint a token with full access
ctxd grant --subject "/**" --operations "read,write,subjects,search"

# Mint a read-only token scoped to /work/**
ctxd grant --subject "/work/**" --operations "read,subjects"
```

The token is output as a base64-encoded string.

## Verification

```bash
ctxd verify --token "<base64>" --subject "/test/hello" --operation read
```

## Attenuation

Tokens can be narrowed via the API. A token for `/**` with `read,write` can be attenuated to `/work/**` with `read` only. The attenuated token is cryptographically bound to the original.

## Caveat types

| Caveat | Description | Example |
|--------|-------------|---------|
| Subject glob | Restricts access to subjects matching a glob pattern | `/work/acme/**` |
| Operation set | Restricts to a set of operations | `read,subjects` |
| Expiry | Token becomes invalid after a timestamp | `2025-02-01T00:00:00Z` |
| Kind | Restrict to specific event types | `ctx.note` |
| Rate limit | Ops/sec cap (enforcement is v0.2) | `100` |

## v0.1 Limitations

- **No revocation.** A minted token is valid until it expires.
- **Expiry is optional.** Default: no expiry.
- **Open by default.** If no token is provided in an MCP tool call, the operation is allowed. Intentional for local development. See [ADR-004](decisions/004-open-by-default.md).
