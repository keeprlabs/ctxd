# Events

ctxd events follow the [CloudEvents v1.0 spec](https://cloudevents.io/) with ctxd-specific extensions.

## Schema

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `specversion` | string | yes | Always `"1.0"` |
| `id` | UUID (v7) | yes | Time-ordered unique identifier |
| `source` | string | yes | Origin of the event (e.g., `"ctxd://localhost:7777"`) |
| `subject` | string | yes | Path where the event is filed (e.g., `"/work/acme/customers/cust-42"`) |
| `type` | string | yes | Event type descriptor (e.g., `"ctx.note"`, `"demo"`) |
| `time` | RFC 3339 | yes | When the event was created |
| `datacontenttype` | string | yes | Content type of `data` (default: `"application/json"`) |
| `data` | JSON | yes | The event payload |
| `predecessorhash` | string | no | SHA-256 hash of the previous event's canonical form |
| `signature` | string | no | Ed25519 signature over canonical form |

## Example

```json
{
  "specversion": "1.0",
  "id": "019756a3-1234-7000-8000-000000000001",
  "source": "ctxd://localhost:7777",
  "subject": "/work/acme/customers/cust-42",
  "type": "ctx.note",
  "time": "2025-01-15T10:30:00Z",
  "datacontenttype": "application/json",
  "data": {
    "content": "Customer mentioned interest in the enterprise plan",
    "author": "user-1"
  },
  "predecessorhash": "a1b2c3d4e5f6..."
}
```

## Canonical Form (for hashing)

The canonical form excludes `predecessorhash` and `signature` to avoid circular dependencies. Keys are sorted alphabetically. The canonical JSON is serialized as bytes, then SHA-256 hashed.

Excluded fields: `predecessorhash`, `signature`
Included fields (sorted): `data`, `datacontenttype`, `id`, `source`, `specversion`, `subject`, `time`, `type`

## Signatures

Events can be signed with Ed25519. The signature covers the same canonical form used for predecessor hashing. This proves the event was produced by the holder of the signing key and has not been modified.

```
Signing flow:
1. Compute canonical form (same as hashing: exclude predecessorhash/signature, sort keys)
2. Serialize to JSON bytes
3. Sign with Ed25519 private key
4. Hex-encode the signature
5. Store in the `signature` field

Verification flow:
1. Compute canonical form of the event
2. Serialize to JSON bytes
3. Verify the hex-decoded signature against the public key
```

The signing key is separate from the capability root key. Capabilities authorize operations. Signatures prove provenance. A single event can have both.

## Hash chain integrity

```
Subject: /work/acme/notes

Event 1: predecessorhash = null (first event in this subject's chain)
Event 2: predecessorhash = SHA256(canonical(Event 1))
Event 3: predecessorhash = SHA256(canonical(Event 2))

Verification: for each event with a predecessorhash, compute the hash of
the previous event and compare. If they don't match, the chain is broken.

Hash chains are per-subject. Events on different subjects have independent chains.
```
