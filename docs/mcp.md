# MCP Tool Surface

ctxd exposes its MCP tool surface over **three transports** that can
run concurrently:

* **stdio** — newline-delimited JSON-RPC on stdin/stdout. Used by
  Claude Desktop, `mcp-inspector`, and other local subprocess clients.
* **SSE (legacy)** — `GET /sse` opens an event stream, `POST
  /messages?sessionId=…` carries JSON-RPC. Used by agent frameworks
  on the pre-2025-03-26 MCP transport.
* **streamable HTTP** — a single `/mcp` endpoint per the MCP
  2025-03-26 / 2025-06-18 spec. Used by Claude.ai and modern hosted
  clients.

All three serve the same tool set with the same return values. See
[Transports](#transports) below for connection examples per transport
and the [auth precedence rules](#auth-precedence).

## Tools

### ctx_write

Append a context event to the store.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `subject` | string | yes | Subject path (e.g., `/work/acme/notes`) |
| `event_type` | string | yes | Event type (e.g., `ctx.note`, `ctx.crm`) |
| `data` | string | yes | Event data as a JSON string |
| `token` | string | no | Base64-encoded capability token |

Returns: `{ "id": "uuid", "subject": "/path", "predecessorhash": "hex..." }`

### ctx_read

Read events for a subject.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `subject` | string | yes | Subject path to read from |
| `recursive` | boolean | no | Include descendant subjects (default: false) |
| `token` | string | no | Base64-encoded capability token |

Returns: JSON array of Event objects, ordered by sequence number.

### ctx_subjects

List known subject paths.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `prefix` | string | no | Only show subjects under this prefix |
| `recursive` | boolean | no | Include descendant subjects (default: false) |
| `token` | string | no | Base64-encoded capability token |

Returns: JSON array of subject path strings.

### ctx_search

Full-text search over event data.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `query` | string | yes | FTS5 search query (e.g., `"enterprise plan"`) |
| `subject_pattern` | string | no | Only search under this subject prefix |
| `k` | integer | no | Max results to return (default: 10) |
| `token` | string | no | Base64-encoded capability token |

Returns: JSON array of matching Event objects, ranked by FTS5 relevance. The `k` limit is applied at the database level (not in application code).

### ctx_subscribe

Poll for events since a timestamp. This is the v0.1 polling mechanism. Real-time streaming via SSE is planned for a future version.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `subject` | string | yes | Subject path to poll |
| `since` | string | no | RFC3339 timestamp. Only return events after this time. If omitted, returns all events. |
| `recursive` | boolean | no | Include descendant subjects (default: false) |
| `token` | string | no | Base64-encoded capability token |

Returns: JSON array of Event objects newer than `since`.

### ctx_entities

Query the graph view for entities.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `entity_type` | string | no | Filter by entity type |
| `name_pattern` | string | no | Search entities by name (SQL LIKE pattern) |
| `subject_pattern` | string | no | Only entities from events under this subject |
| `token` | string | no | Base64-encoded capability token |

Returns: JSON array of Entity objects.

### ctx_related

Query the graph view for relationships.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `entity_id` | string | yes | The entity to find relationships for |
| `relationship_type` | string | no | Filter by relationship type |
| `token` | string | no | Base64-encoded capability token |

Returns: JSON array of `{ "relationship": {...}, "entity": {...} }` pairs.

### ctx_timeline

Query historical state at a point in time.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `subject` | string | yes | Subject path |
| `as_of` | string | yes | RFC3339 timestamp. Returns state as of this time. |
| `recursive` | boolean | no | Include descendant subjects (default: false) |
| `token` | string | no | Base64-encoded capability token |

Returns: JSON array of events that existed at the given point in time (events with `time <= as_of`).

## Transports

`ctxd serve` accepts three flags controlling which transports are
active. Stdio is on by default; the HTTP transports are off until
their address flag is set.

```bash
ctxd serve \
  --mcp-stdio \                # default true
  --mcp-sse 127.0.0.1:7779 \   # off by default
  --mcp-http 127.0.0.1:7780    # off by default
```

Add `--require-auth` to reject unauthenticated `tools/call` requests
on the HTTP transports (stdio is unaffected).

### stdio

Newline-delimited JSON-RPC on stdin/stdout. The daemon's lifetime is
tied to the parent process — when the parent disconnects, only the
stdio task ends; sibling SSE / HTTP transports keep serving.

#### Claude Desktop

Add to `claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "ctxd": {
      "command": "ctxd",
      "args": ["serve", "--mcp-stdio"]
    }
  }
}
```

#### mcp-inspector

```bash
npx @anthropic-ai/mcp-inspector ctxd serve --mcp-stdio
```

#### Cursor

Add to `.cursor/mcp.json`:

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

#### Auth

Tokens travel as the per-tool-call `token` argument. There is no
header on stdio. Stdio is the legacy "open by default" transport;
absent a token the call is allowed.

### SSE (legacy)

The pre-2025-03-26 MCP HTTP transport. Two endpoints:

* `GET /sse` — open an `text/event-stream` response. The first event
  is `endpoint`, whose `data` field is a relative URL like
  `/messages?sessionId=…` that subsequent JSON-RPC POSTs must target.
  All server→client responses are emitted as `message` SSE events on
  this stream.
* `POST /messages?sessionId=…` — JSON-RPC body. Returns 202 Accepted;
  the response flows back over the SSE stream.

#### Connection example

```bash
# Open the event stream (terminal 1)
curl -N -H 'Accept: text/event-stream' http://127.0.0.1:7779/sse

# event: endpoint
# data: /messages?sessionId=abc…

# Send a JSON-RPC call (terminal 2)
curl -X POST 'http://127.0.0.1:7779/messages?sessionId=abc…' \
  -H 'Content-Type: application/json' \
  -H 'Authorization: Bearer <base64-biscuit>' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}'
```

#### Typical clients

Custom agent frameworks built before MCP 2025-03-26 — anything that
expects the `endpoint`/`message` SSE pair.

#### Auth

Either `Authorization: Bearer <base64-biscuit>` header on the
`/messages` POST, or the per-call `token` argument. **Header wins
when both are present** — see [auth precedence](#auth-precedence).

### streamable HTTP

The MCP 2025-03-26 / 2025-06-18 single-endpoint transport. One route:

* `POST /mcp` — JSON-RPC request. By default we run in **stateless,
  JSON-response** mode: the response is a regular `application/json`
  body. Clients that prefer SSE streaming can negotiate
  `Accept: text/event-stream`.

#### Connection example

```bash
curl -X POST http://127.0.0.1:7780/mcp \
  -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' \
  -H 'Authorization: Bearer <base64-biscuit>' \
  -d '{
    "jsonrpc":"2.0",
    "id":1,
    "method":"tools/call",
    "params":{
      "name":"ctx_read",
      "arguments":{"subject":"/work/acme","recursive":true}
    }
  }'
```

#### Typical clients

Claude.ai, Anthropic's hosted agents, agent frameworks targeting MCP
2025-03-26 or later.

#### Auth

Same as SSE: `Authorization: Bearer …` header preferred, per-call
`token` argument honoured for parity. Header wins on conflict.

## Auth precedence

When both an `Authorization: Bearer <token>` header and a per-call
`token` argument are presented, **the header wins** and the argument
is silently ignored. The cap engine sees only the header token.

This is enforced inside ctxd's HTTP middleware: when a header is
present, the JSON-RPC body's `params.arguments.token` field is
overwritten before the request reaches the rmcp service. The tool
handlers themselves never see the conflict — they receive a single
optional `token` value.

The header value is **never logged**, in any tracing field. The
middleware sanitises the header itself: only ASCII bytes are
accepted, the prefix must be the exact literal `Bearer ` (RFC 6750,
case-sensitive), and embedded whitespace inside the token rejects
the call. Malformed `Authorization` headers are treated the same as
"no header" — they do not, on their own, return 401 unless
`--require-auth` is on AND no per-call token argument is present
either.

## `--require-auth`

By default, the HTTP transports inherit stdio's "open by default"
behaviour: a `tools/call` with no token reaches the cap engine,
which only enforces a token's *contents* when one is supplied.

Setting `--require-auth` flips that policy on the HTTP transports
only — every `tools/call` must carry a token, header or arg. Calls
without one return `401 Unauthorized` from the middleware before the
rmcp service is invoked. Stdio is unaffected: subprocess clients are
local-trust by definition.

## TLS

**ctxd does not ship TLS in-process.** Production deployments must
front the HTTP transports with a reverse proxy (nginx, Caddy, AWS
ALB, …) that terminates TLS and forwards plaintext HTTP to the
daemon.

This is a deliberate decision — see
[ADR 013](decisions/013-multi-transport-mcp.md). For loopback-only
deployments (the daemon and its consumers on the same host) the
proxy is unnecessary; TLS adds nothing on `127.0.0.1`.

## DNS rebinding protection

The streamable-HTTP transport rejects requests whose `Host` header is
not in an allow-list. The default list is `localhost`, `127.0.0.1`,
and `::1` — sufficient for loopback deployments. If you bind to a
non-loopback address (e.g. `0.0.0.0:7780` so a reverse proxy can
forward in), the proxy must rewrite the `Host` header to one of those
values, OR you can extend the allow-list at startup (see
`StreamableHttpServerConfig::with_allowed_hosts` in the rmcp source —
this is currently a code-level configuration; a CLI flag is on the
roadmap).

The SSE transport does not enforce this check today — its `Host`
validation lives upstream of any proxy you put in front of it.

## Body size limits

The HTTP transports cap inbound JSON-RPC bodies at **1 MiB per
request** (`DEFAULT_MAX_BODY_BYTES`). Larger payloads return
`413 Payload Too Large`. This is a defence against DoS — a hostile
or buggy client cannot exhaust daemon memory by sending an
arbitrarily large `tools/call`. The threshold is configurable from
library callers via `AuthMiddlewareConfig::max_body_bytes`.

## Authorization

Every tool accepts an optional `token` parameter. When provided, the
token is verified against the requested operation and subject before
proceeding.

When no token is provided and `--require-auth` is *not* set, the
operation is allowed. This is the v0.1 stdio default. To require
tokens on all HTTP-transport calls, set `--require-auth`.

Tokens are base64-encoded biscuit tokens. Mint them with `ctxd
grant` or `POST /v1/grant`.

## Error handling

Tool calls never fail at the MCP protocol level. Errors are returned as content in the tool result:

- Authorization failures: `"error: authorization denied: ..."`
- Invalid subjects: `"error: invalid subject: ..."`
- Store errors: `"error: read failed: ..."` / `"error: write failed: ..."`

The MCP client (Claude, Cursor) will see these as text content and can interpret them.
