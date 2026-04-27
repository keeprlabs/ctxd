# ctxd Wire Protocol (v0.3)

This document is the source-of-truth specification for the ctxd
client-to-daemon wire protocol. SDKs in three languages (Rust, Python,
TS/JS) generate types from this document and the companion
`events.schema.json`.

The protocol is deliberately tiny: a length-prefixed framing layer
wrapping externally-tagged MessagePack payloads. There is no
handshake, no version negotiation, no compression. The transport is
TCP; TLS is out of scope for v0.3 and is handled by SSH / WireGuard /
similar at deployment time.

The reference Rust implementation lives at
`crates/ctxd-wire/src/{frame,messages}.rs`. This document tracks that
implementation byte-for-byte: when they disagree, the implementation
wins and this document is wrong.

## 1. Framing

```
+-----------------+----------------------+
| length: u32 BE  | payload: msgpack     |
| (4 bytes)       | (length bytes)       |
+-----------------+----------------------+
```

- The first 4 bytes are a big-endian unsigned 32-bit integer giving the
  byte length of the payload that follows.
- The next `length` bytes are a single MessagePack-encoded value (the
  Request, Response, or BroadcastEvent below).
- Frames concatenate end-to-end on the same TCP connection. There is
  no record separator or trailer.
- A clean TCP close at a frame boundary (i.e. before the next length
  prefix) is the normal end-of-stream signal.

### Limits

- **Maximum frame size:** `16 MiB` (`16 * 1024 * 1024 = 16_777_216`
  bytes). A reader that observes a length prefix larger than this
  MUST reject the frame *before* allocating the buffer (the reference
  implementation returns `WireError::FrameTooLarge`).
- The 16 MiB ceiling is a defense-in-depth measure: it puts a hard
  upper bound on the memory a hostile peer can force you to allocate
  with a single 4-byte header.

### Reference

- `crates/ctxd-wire/src/frame.rs`:
  - `pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;`
  - `read_frame` / `write_frame`.

## 2. Encoding

The payload is MessagePack, produced by `rmp-serde` with the default
serde encoding rules. Important consequences:

- **Externally-tagged enums.** A `Request::Pub { ... }` is encoded as
  a MessagePack map with a single key `"Pub"` whose value is the
  inner struct. A nullary variant such as `Request::Ping` is encoded
  as the bare string `"Ping"`.
- **Structs as positional arrays.** `rmp-serde` encodes named-field
  structs and struct-form enum variants as MessagePack *arrays*, in
  field declaration order — not as maps. So the inner payload of
  `Request::Pub { subject, event_type, data }` is a 3-element array
  `[subject, event_type, data]`, NOT a map `{"subject": ..., ...}`.
  This is the rmp-serde default and is the canonical wire format.
  SDKs MUST mirror this ordering byte-for-byte; the conformance
  corpus pins it.
- **Field ordering.** As above, struct fields are emitted in
  declaration order. The order is locked in `crates/ctxd-wire/src/messages.rs`
  and is part of the wire contract.
- **Optional fields.** `Option<T>` is encoded as `nil` when `None`
  and the inner value when `Some(T)`. There is no key omission for
  None at the wire-protocol layer (event payloads, separately, follow
  CloudEvents conventions and DO omit absent optional keys — see
  `events.schema.json`).
- **Endianness.** MessagePack defines its own per-type endianness;
  `rmp-serde` follows the spec. The framing length prefix is the only
  bare integer in the protocol and is big-endian.
- **String type.** All strings are MessagePack `str` (UTF-8). Byte
  vectors (e.g. `PeerHello.public_key`) are MessagePack `bin`.

### Reference

- `crates/ctxd-wire/src/messages.rs`.
- `rmp-serde = 1.x` is the only msgpack codec the daemon supports;
  SDKs that pick a different codec are responsible for matching its
  externally-tagged enum encoding byte-for-byte.

## 3. Verbs

There are 13 request variants. The first 6 are the SDK surface; the
remaining 7 are federation verbs **internal between daemons**. SDK
clients MUST NOT send federation verbs; doing so will either be
rejected with a `Response::Error` or, in some federation-disabled
builds, ignored. Calling them from an SDK is a wire-contract
violation.

### 3.1 SDK verbs

| Verb | Args | Successful response | Error semantics |
| --- | --- | --- | --- |
| `Pub` | `subject: string`, `event_type: string`, `data: any` | `Ok { data: { id, predecessorhash } }` | `Error { message }` for capability denial, store error, or malformed subject. |
| `Sub` | `subject_pattern: string` | Stream of `Event { event }` followed by `EndOfStream` (on close) | `Error` if the pattern is invalid or the cap doesn't grant `read`. |
| `Query` | `subject_pattern: string`, `view: "log"\|"kv"\|"fts"` | `Ok { data: <view-specific> }` | `Error` for unknown view name or capability denial. |
| `Grant` | `subject: string`, `operations: string[]`, `expiry: string?` | `Ok { data: { token } }` (base64 biscuit) | `Error` for unknown operation token, invalid expiry, or admin-cap denial. |
| `Revoke` | `cap_id: string` | `Ok { data: { revoked: true } }` | `Error` if the cap id is unknown (v0.2 stub — semantics may tighten). |
| `Ping` | (no args) | `Pong` | none — `Ping` is the liveness probe and never carries an `Error` path. |

### 3.2 Federation verbs (daemon-to-daemon only)

| Verb | Args | Notes |
| --- | --- | --- |
| `PeerHello` | `peer_id`, `public_key (32 bytes)`, `offered_cap (b64)`, `subjects` | First message after a federation TCP connect. |
| `PeerWelcome` | `peer_id`, `public_key`, `offered_cap`, `subjects` | Reply to `PeerHello`. |
| `PeerReplicate` | `origin_peer_id`, `event` (CloudEvents JSON) | Streaming replication — forwarded once for every event the peer is allowed to receive. |
| `PeerAck` | `origin_peer_id`, `event_id (UUIDv7 string)` | Acknowledgement; advances the receiver's cursor. |
| `PeerCursorRequest` | `peer_id`, `subject_pattern` | Resume-from-cursor query after disconnect. |
| `PeerCursor` | `peer_id`, `subject_pattern`, `last_event_id?`, `last_event_time?` | Reply to `PeerCursorRequest`. |
| `PeerFetchEvents` | `event_ids: string[]` | Backfill request when an inbound `PeerReplicate` references parents we don't yet have. |

The five `Response` variants are:

| Variant | When | Payload |
| --- | --- | --- |
| `Ok { data }` | Successful unary call. | The shape depends on the verb (see table above). |
| `Event { event }` | Streaming event from `Sub` or replication. | A serialized `Event` (see `events.schema.json`). |
| `Error { message }` | Anything went wrong. | A human-readable message. SDKs SHOULD surface this verbatim and not parse it for control flow. |
| `Pong` | Reply to `Ping`. | (none) |
| `EndOfStream` | A streaming response (e.g. `Sub`) is closing. | (none) |

## 4. Worked example: `Pub` round-trip

The fixture `docs/api/conformance/wire/pub_request.json` contains the
following logical `Request`:

```json
{
  "Pub": {
    "subject": "/test/hello",
    "event_type": "demo",
    "data": { "msg": "world" }
  }
}
```

`rmp-serde` encodes this to the bytes in
`docs/api/conformance/wire/pub_request.msgpack.hex`. Annotated:

```
81                                # fixmap (1 entry: variant tag)
  a3 50 75 62                     #   "Pub" (str3)
  93                              #   fixarray (3 entries — positional struct fields)
    ab 2f 74 65 73 74 2f 68 65 6c 6c 6f  # "/test/hello"   (subject)
    a4 64 65 6d 6f                       # "demo"          (event_type)
    81                                   # fixmap (1 entry — payload object)
      a3 6d 73 67                          # "msg"
      a5 77 6f 72 6c 64                    # "world"
```

Wrapped in the framing layer (length prefix `0x00000022 = 34`):

```
00 00 00 22 81 a3 50 75 62 93 ab 2f 74 65 73 74 2f 68 65 6c 6c 6f a4 64 65 6d 6f
81 a3 6d 73 67 a5 77 6f 72 6c 64
```

The daemon's response — assuming the cap grants `write` — is a
`Response::Ok` whose `data` is `{ "id": "<uuidv7>",
"predecessorhash": "<hex>" }`. The conformance fixture pins a
deterministic example (`ok_response.{json,msgpack.hex}`) using a
fixed UUID and hash so the encoding is byte-stable.

## 5. Stability and versioning

- The wire protocol carries no version field. The daemon's HTTP
  `/health` endpoint reports the daemon's package version; SDKs check
  compatibility against the table in `COMPATIBILITY.md`.
- Adding a new `Request` or `Response` variant is a non-breaking
  change for older SDKs that ignore unknown variants — but `rmp-serde`
  does NOT silently ignore unknown variants by default, so every
  existing SDK release pin lists the daemon versions it accepts.
- Renaming a variant or reshaping a struct is a breaking change. It
  requires a major bump for `ctxd-core`, `ctxd-wire`, and every SDK.
- Conformance test corpus (`docs/api/conformance/wire/`) catches drift
  early: any change to canonical msgpack output makes
  `cargo test -p ctxd-wire --test conformance_corpus` fail.
