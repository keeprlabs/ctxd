# Events

ctxd events follow the [CloudEvents v1.0 spec](https://cloudevents.io/) with ctxd-specific extensions.

## Schema

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `specversion` | string | yes | Always `"1.0"` |
| `id` | UUID (v7) | yes | Time-ordered unique identifier |
| `source` | string | yes | Origin of the event (e.g., `"ctxd://localhost:7777"`) |
| `subject` | string | yes | Path where the event is filed (e.g., `"/work/exlo/customers/dmitry"`) |
| `type` | string | yes | Event type descriptor (e.g., `"ctx.note"`, `"demo"`) |
| `time` | RFC 3339 | yes | When the event was created |
| `datacontenttype` | string | yes | Content type of `data` (default: `"application/json"`) |
| `data` | JSON | yes | The event payload |
| `predecessorhash` | string | no | SHA-256 hash of the previous event's canonical form |
| `signature` | string | no | Ed25519 signature (reserved for v0.2) |

## Example

```json
{
  "specversion": "1.0",
  "id": "019756a3-1234-7000-8000-000000000001",
  "source": "ctxd://localhost:7777",
  "subject": "/work/exlo/customers/dmitry",
  "type": "ctx.note",
  "time": "2025-01-15T10:30:00Z",
  "datacontenttype": "application/json",
  "data": {
    "content": "Dmitry mentioned interest in the enterprise plan",
    "author": "alice"
  },
  "predecessorhash": "a1b2c3d4e5f6..."
}
```

## Canonical Form (for hashing)

The canonical form excludes `predecessorhash` and `signature` to avoid circular dependencies. Keys are sorted alphabetically. The canonical JSON is serialized as bytes, then SHA-256 hashed.

Excluded fields: `predecessorhash`, `signature`
Included fields (sorted): `data`, `datacontenttype`, `id`, `source`, `specversion`, `subject`, `time`, `type`
