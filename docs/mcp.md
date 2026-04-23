# MCP Tool Surface

ctxd exposes MCP tools over stdio transport. Connect via Claude Desktop, mcp-inspector, or any MCP client.

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

### ctx_entities (v0.2)

Query the graph view for entities.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `entity_type` | string | no | Filter by entity type |
| `name_pattern` | string | no | Search entities by name (SQL LIKE pattern) |
| `subject_pattern` | string | no | Only entities from events under this subject |
| `token` | string | no | Base64-encoded capability token |

Returns: JSON array of Entity objects.

### ctx_related (v0.2)

Query the graph view for relationships.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `entity_id` | string | yes | The entity to find relationships for |
| `relationship_type` | string | no | Filter by relationship type |
| `token` | string | no | Base64-encoded capability token |

Returns: JSON array of `{ "relationship": {...}, "entity": {...} }` pairs.

### ctx_timeline (v0.2)

Query historical state at a point in time.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `subject` | string | yes | Subject path |
| `as_of` | string | yes | RFC3339 timestamp. Returns state as of this time. |
| `recursive` | boolean | no | Include descendant subjects (default: false) |
| `token` | string | no | Base64-encoded capability token |

Returns: JSON array of events that existed at the given point in time (events with `time <= as_of`).

## Connecting

### Claude Desktop

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

### mcp-inspector

```bash
npx @anthropic-ai/mcp-inspector ctxd serve --mcp-stdio
```

### Cursor

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

## Authorization

Every tool accepts an optional `token` parameter. When provided, the token is verified against the requested operation and subject before proceeding.

When no token is provided, the operation is allowed. This is the default for local development. To require tokens on all operations, the daemon will support a `--require-auth` flag in a future version.

Tokens are base64-encoded biscuit tokens. Mint them with `ctxd grant` or `POST /v1/grant`.

## Error handling

Tool calls never fail at the MCP protocol level. Errors are returned as content in the tool result:

- Authorization failures: `"error: authorization denied: ..."`
- Invalid subjects: `"error: invalid subject: ..."`
- Store errors: `"error: read failed: ..."` / `"error: write failed: ..."`

The MCP client (Claude, Cursor) will see these as text content and can interpret them.
