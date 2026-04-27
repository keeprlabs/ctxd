# @ctxd/client

Official TypeScript / JavaScript SDK for [ctxd](https://github.com/keeprlabs/ctxd) — the
context substrate for AI agents.

`@ctxd/client` is the thin, opinionated wrapper around the daemon's
public API surface: HTTP admin (`/health`, `/v1/grant`, `/v1/peers`,
`/v1/stats`) and the wire protocol (MessagePack over TCP — write,
query, subscribe). One client class, three lines to get going,
isomorphic where it can be (HTTP) and Node-only where the runtime
demands it (raw TCP).

```bash
npm install @ctxd/client
```

## Quickstart (Node)

```ts
import { CtxdClient, Operation } from "@ctxd/client";

const client = new CtxdClient({
  httpUrl: "http://127.0.0.1:7777",
  wireAddr: "127.0.0.1:7778",
});

try {
  const eid = await client.write({
    subject: "/work/notes/standup",
    eventType: "ctx.note",
    data: { content: "ship Friday" },
  });

  const events = await client.query({ subjectPattern: "/work/notes" });
  console.log(events.find((e) => e.id === eid));

  const token = await client.grant({
    subject: "/work/notes/**",
    operations: [Operation.Read, Operation.Subjects],
  });
  console.log(`token: ${token}`);
} finally {
  await client.close();
}
```

## Quickstart (browser)

The browser bundle is **HTTP-only**. The wire protocol speaks raw TCP
+ MessagePack and there's no path to that from a browser sandbox.
Calling wire-protocol methods (`write`, `query`, `subscribe`,
`revoke`) from the browser bundle throws `WireError`. HTTP admin
methods work as on Node.

```ts
import { CtxdClient } from "@ctxd/client";

// Browser bundlers will pick up dist/index.browser.js automatically
// via the package.json `browser` export condition.
const client = new CtxdClient({
  httpUrl: "https://my-ctxd.example.com",
  token: "...biscuit-token...",
});

const health = await client.health();
console.log(health.version); // "0.3.0"
```

If you need real-time subscriptions in a browser, a v0.4 follow-up
will translate `subscribe()` to the `ctx_subscribe` MCP polling
endpoint. For v0.3, run a Node sidecar.

## What's in the box

| API | Path | Notes |
|-----|------|-------|
| `new CtxdClient({...})` | constructor | HTTP is required, wire is optional but needed for `write` / `subscribe` / `query` / `revoke`. |
| `health`, `stats` | HTTP | Open by default. |
| `write` | wire | Append an event under a subject. Returns the new UUIDv7 id. |
| `subscribe` | wire | Async iterator (`for await ... of client.subscribe(...)`). |
| `query` | wire | `view: "log"` and `view: "fts"` return parsed `Event` lists. |
| `grant` | HTTP | Mints a base64-encoded biscuit token. |
| `revoke` | wire | Wire `Revoke` verb. (HTTP revoke is on the v0.4 roadmap.) |
| `peers`, `peerRemove` | HTTP, admin | Requires a token with `Operation.Admin`. |
| `verifySignature` | pure fn | Ed25519 over canonical bytes; matches daemon byte-for-byte via the `docs/api/conformance/signatures/*.json` corpus. |

## Subscriptions

`subscribe()` is an async generator. The daemon puts the underlying
TCP connection into streaming-receive mode after a `Sub`, so the SDK
opens a fresh connection per subscription:

```ts
for await (const event of client.subscribe("/work/**")) {
  console.log(event.type, event.subject);
}
```

The generator's TCP connection is closed when the loop exits (whether
naturally or via `break`/`return`).

## Verify a signed event

```ts
import { verifySignature } from "@ctxd/client";

const ok = await verifySignature(event, pubkeyHex);
```

`verifySignature` is async because `@noble/ed25519` v2's verify
primitives are async. The SDK wires the SHA-512 hook noble v2 needs
at module load (using `@noble/hashes/sha512`) — you don't have to
configure anything yourself.

## Errors

Every error thrown by the SDK extends `CtxdError`. Subclasses tell
you which layer failed:

| Class | Thrown when |
|-------|-------------|
| `HttpError` | HTTP request returned a non-2xx (carries `status` + `body`). |
| `AuthError` | HTTP 401 / 403. |
| `NotFoundError` | HTTP 404. |
| `WireError` | TCP IO or codec failure. |
| `WireNotConfiguredError` | Tried to call a wire method without `wireAddr`. |
| `UnexpectedWireResponseError` | Daemon's response shape didn't match the SDK's expectation. |
| `SigningError` | Malformed pubkey hex or wrong-length pubkey. |

## Logging

The SDK never logs bearer tokens, capability bytes, or signature
material. Cryptographic verify failures resolve to `false` (matching
the Rust + Python SDKs) — they never reject with a stack trace that
would leak side-channel information.

## How it relates to the rest of ctxd

This package lives at `clients/typescript/ctxd-client/` in the ctxd
workspace and consumes the `docs/api/` contract artifact (the OpenAPI
spec, the events JSON Schema, and the wire/signature/event
conformance corpus). The Rust SDK
(`clients/rust/ctxd-client`) is the reference shape; the Python and
TypeScript packages mirror it.

- [ctxd root README](../../../README.md)
- [HTTP API contract (OpenAPI)](../../../docs/api/openapi.yaml)
- [Wire protocol spec](../../../docs/api/wire-protocol.md)
- [Event schema](../../../docs/api/events.schema.json)

## Supported runtimes

- Node 20+ (LTS).
- Bun 1.1+.
- Modern browsers (HTTP-only subset).

## License

Apache-2.0.
