# ctxd-client

Official Python SDK for [ctxd](https://github.com/keeprlabs/ctxd) — the
context substrate for AI agents.

`ctxd-client` is the thin, opinionated wrapper around the daemon's
public API surface: HTTP admin (`/health`, `/v1/grant`, `/v1/peers`,
`/v1/stats`) and the wire protocol (MessagePack over TCP — write,
query, subscribe). One client class, three lines to get going, no
hidden runtimes or system crypto baggage.

The PyPI package is named `ctxd-client`, but it imports as `ctxd` so
your code reads naturally:

```bash
pip install ctxd-client
```

```python
import ctxd
```

## Quickstart (async)

```python
import asyncio
from ctxd import CtxdAsyncClient, Operation

async def main() -> None:
    async with CtxdAsyncClient.connect("http://127.0.0.1:7777") as client:
        await client.with_wire("127.0.0.1:7778")

        eid = await client.write(
            "/work/notes/standup",
            "ctx.note",
            {"content": "ship Friday"},
        )

        events = await client.query("/work/notes", view="log")
        assert any(e.id == eid for e in events)

        token = await client.grant(
            "/work/notes/**",
            [Operation.READ, Operation.SUBJECTS],
        )
        print(f"token: {token}")

asyncio.run(main())
```

## Quickstart (sync)

For scripts and CLIs, the synchronous facade mirrors the async API
method-for-method. It owns a long-lived event loop on a background
thread, so you don't fight with an ambient asyncio loop in your host
application:

```python
from ctxd import CtxdClient

with CtxdClient.connect("http://127.0.0.1:7777") as client:
    client.with_wire("127.0.0.1:7778")
    info = client.health()
    print(info.version)
```

The sync wrapper is **not** optimal for concurrent workloads — every
call serializes on the background loop. For high-throughput pipelines,
use `CtxdAsyncClient` directly.

## What's in the box

| API | Path | Notes |
|-----|------|-------|
| `connect`, `with_wire`, `with_token` | constructor | HTTP is required, wire is optional but needed for `write` / `subscribe` / `query` / `revoke`. |
| `health`, `stats` | HTTP | Open by default. |
| `write` | wire | Append an event under a subject. Returns the new UUIDv7 id. |
| `subscribe` | wire | Async iterator (`async for ... in client.subscribe(...)`). |
| `query` | wire | `view="log"` and `view="fts"` return parsed `Event` lists. |
| `grant` | HTTP | Mints a base64-encoded biscuit token. |
| `revoke` | wire | Wire `Revoke` verb. (HTTP revoke is on the v0.4 roadmap.) |
| `peers`, `peer_remove` | HTTP, admin | Requires a token with `Operation.ADMIN`. |
| `verify_signature` | pure fn | Ed25519 over canonical bytes; matches daemon byte-for-byte via the `docs/api/conformance/signatures/*.json` corpus. |

## Subscriptions

`subscribe()` returns an async iterator pinned to a fresh TCP
connection (the daemon puts the socket into streaming-receive mode
after a `Sub`, so it can't be reused for further requests):

```python
async for event in await client.subscribe("/work/**"):
    print(event.event_type, event.subject)
```

The sync wrapper exposes the same with a regular iterator:

```python
for event in client.subscribe("/work/**"):
    ...
```

## Logging

The SDK logs to the stdlib `logging` logger named `ctxd` at `DEBUG`.
**No bearer tokens, capability bytes, or signature material are ever
logged.** Configure via the standard `logging` API:

```python
import logging
logging.getLogger("ctxd").setLevel(logging.DEBUG)
```

## How it relates to the rest of ctxd

This package lives at `clients/python/ctxd-py/` in the ctxd workspace
and consumes the `docs/api/` contract artifact (the OpenAPI spec, the
events JSON Schema, and the wire/signature/event conformance corpus).
The Rust SDK (`clients/rust/ctxd-client`) is the reference shape; this
package mirrors it.

- [ctxd root README](../../../README.md)
- [HTTP API contract (OpenAPI)](../../../docs/api/openapi.yaml)
- [Wire protocol spec](../../../docs/api/wire-protocol.md)
- [Event schema](../../../docs/api/events.schema.json)

## Supported Python versions

3.10, 3.11, 3.12, 3.13.

## License

Apache-2.0.
