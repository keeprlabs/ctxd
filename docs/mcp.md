# MCP Tool Surface

ctxd exposes three MCP tools over stdio transport. Connect via Claude Desktop, mcp-inspector, or any MCP client.

## Tools

### ctx_write

Append a context event to the store.

**Parameters:**

| Name | Type | Required | Description |
|------|------|----------|-------------|
| `subject` | string | yes | Subject path (e.g., `/test/hello`) |
| `event_type` | string | yes | Event type descriptor |
| `data` | string | yes | Event data as a JSON string |
| `token` | string | no | Base64-encoded capability token |

**Returns:** JSON with `id`, `subject`, and `predecessorhash`.

### ctx_read

Read context events for a subject.

**Parameters:**

| Name | Type | Required | Description |
|------|------|----------|-------------|
| `subject` | string | yes | Subject path to read from |
| `recursive` | boolean | no | Whether to read descendants (default: false) |
| `token` | string | no | Base64-encoded capability token |

**Returns:** JSON array of events.

### ctx_subjects

List known subject paths.

**Parameters:**

| Name | Type | Required | Description |
|------|------|----------|-------------|
| `prefix` | string | no | Filter subjects under this prefix |
| `recursive` | boolean | no | Whether to list descendants (default: false) |
| `token` | string | no | Base64-encoded capability token |

**Returns:** JSON array of subject path strings.

## Connecting

### Claude Desktop

Add to your `claude_desktop_config.json`:

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

## Authorization

If a `token` parameter is provided, it is verified before the operation is performed. If no token is provided, the operation is allowed (v0.1 default for local development).
