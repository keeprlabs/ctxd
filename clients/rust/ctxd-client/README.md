# ctxd-client

Official Rust SDK for [ctxd](https://github.com/keeprlabs/ctxd) — the
context substrate for AI agents.

`ctxd-client` is the thin, opinionated wrapper around the daemon's
public API surface: HTTP admin (`/health`, `/v1/grant`, `/v1/peers`,
`/v1/stats`) and the wire protocol (MessagePack over TCP — write,
query, subscribe). One client type, three lines to get going, no
hidden tokio runtimes or `native-tls` baggage.

## Install

```toml
[dependencies]
ctxd-client = "0.3"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

Or via cargo:

```bash
cargo add ctxd-client
```

## Quickstart

```rust,no_run
use ctxd_client::{CtxdClient, Operation, QueryView};

#[tokio::main]
async fn main() -> Result<(), ctxd_client::CtxdError> {
    let client = CtxdClient::connect("http://127.0.0.1:7777").await?
        .with_wire("127.0.0.1:7778").await?;

    // Append an event.
    let id = client
        .write("/work/notes/standup", "ctx.note",
               serde_json::json!({"content": "ship Friday"}))
        .await?;

    // Read it back.
    let events = client.query("/work/notes", QueryView::Log).await?;
    assert!(events.iter().any(|e| e.id == id));

    // Mint a scoped, read-only capability for an agent.
    let token = client
        .grant("/work/notes/**",
               &[Operation::Read, Operation::Subjects],
               None)
        .await?;
    println!("token: {token}");

    Ok(())
}
```

## What's in the box

| API | Path | Notes |
|-----|------|-------|
| `connect`, `with_wire`, `with_token` | constructor | HTTP is required, wire is optional but needed for `write`/`subscribe`/`query`/`revoke`. |
| `health`, `stats` | HTTP | Open by default. |
| `write` | wire | Append an event under a subject. |
| `subscribe` | wire | Returns an `EventStream` you can `.next_event().await`. |
| `query` | wire | `QueryView::Log` and `QueryView::Fts` return parsed `Event` lists. |
| `grant` | HTTP | Mints a base64-encoded biscuit. |
| `revoke` | wire | Wire `Revoke` verb. (HTTP revoke is on the v0.4 roadmap.) |
| `peers`, `peer_remove` | HTTP, admin | Requires a token with `Operation::Admin`. |
| `verify_signature` | pure fn | Ed25519 over canonical bytes; matches daemon byte-for-byte via the `docs/api/conformance/signatures/*.json` corpus. |

## TLS

HTTPS support is `rustls`-only. We deliberately do **not** pull in
`native-tls` / `openssl` — the SDK should not require system crypto
libraries on its host. If you need `native-tls`, file an issue with
your use case.

## Subscriptions

`subscribe` returns an `EventStream` rather than implementing
`futures::Stream` directly. Use the bare async method:

```rust,no_run
# use ctxd_client::CtxdClient;
# async fn run(client: CtxdClient) -> Result<(), ctxd_client::CtxdError> {
let mut stream = client.subscribe("/work/**").await?;
while let Some(event) = stream.next_event().await? {
    println!("{} on {}", event.event_type, event.subject);
}
# Ok(()) }
```

A `futures::Stream` impl is on the v0.4 roadmap once
`async-fn-in-trait`-shaped streams stabilize the pattern.

## Workspace layout

This crate lives at `clients/rust/ctxd-client/` in the ctxd workspace
and depends on the lean leaf crates:

- `ctxd-core` — for the `Event` and `Subject` types.
- `ctxd-wire` — for the protocol shape and `ProtocolClient`.

It does **not** depend on `ctxd-store`, `ctxd-cap`, `ctxd-http`, axum,
or sqlx. The whole point of an SDK is that you can take a dependency
on it from any Rust app without inheriting a server.

## MSRV

Rust 1.78.

## License

Apache-2.0.
