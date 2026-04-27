# Client SDK Plan

**Status:** approved by Mani 2026-04-26. All defaults from the draft locked in **except**:

- **Types are generated from the API contract**, not hand-written (decision #4).
- **Ed25519 signature verification ships in all four SDKs in v1**, not just Rust+Python (decision #8).

This document is the source of truth for the v1 SDK milestone. It supersedes any conflicting notes in conversation.

## 1. Goals & non-goals

### Goals

Ship official client SDKs in **Rust, Python, TypeScript/JavaScript, and Java** that let application developers talk to a `ctxd serve` instance without writing transport plumbing. Primary audience: services that ingest into ctxd or query it. Secondary: adapter authors and admin/observability tooling, which fall out of the same API.

### Non-goals (v1)

- Not reimplementing the daemon.
- Not shipping federation peer behavior. Wire-protocol federation verbs (`PEER_HELLO`, `PEER_REPLICATE`, …) are daemon-only by policy. Letting random clients pretend to be peers would destabilize the network.
- Not shipping an MCP server or client. Use `rmcp` / `@modelcontextprotocol/sdk` and friends; point them at `ctxd serve --mcp-http`.
- Not shipping TLS termination. Per ADR 013, production runs behind a reverse proxy.

## 2. API surface

| Transport | Rust | Python | TS/JS | Java | Notes |
|---|---|---|---|---|---|
| HTTP admin (`/v1/*`) | ✅ | ✅ | ✅ | ✅ | Universal. |
| Wire protocol (TCP+MsgPack) | ✅ | ✅ | Node-only | ✅ | Browser TCP impossible; browser gets HTTP only. |
| Real-time `SUB` | ✅ | ✅ | Node ✅ / browser polling | ✅ | Browser polls via HTTP `since` semantics. WS bridge deferred to v2. |
| MCP transports | — | — | — | — | Out of scope. |
| Federation verbs | — | — | — | — | Out of scope, never in v1. |
| Ed25519 sig verify | ✅ | ✅ | ✅ | ✅ | All four SDKs (decision #8). |

**Approval-gated writes** can block up to 5 minutes (`docs/mcp.md`). Every SDK's HTTP write path defaults to a 6-minute timeout with a knob to override.

**Documentation discrepancy to resolve before shipping:** the v0.3 README and CHANGELOG mention `GET /v1/peers` and `DELETE /v1/peers/:id`, but those endpoints do not exist in `crates/ctxd-http/src/router.rs`. Two options: (a) add them to the daemon as part of the SDK milestone, (b) correct the docs. Decision before we publish the OpenAPI spec.

## 3. Types: generated from the API contract

The contract artifact in `docs/api/` is the source of truth. SDK types are **generated** from it on every CI run; drift between SDK and spec fails CI.

| Artifact | Generators per language |
|---|---|
| `docs/api/openapi.yaml` (HTTP admin) | Rust: `progenitor`. Python: `openapi-python-client`. TS: `openapi-typescript` + `openapi-fetch`. Java: `openapi-generator` (`java`/`jsr-303` modes). |
| `docs/api/events.schema.json` (CloudEvents + ctxd extensions) | Rust: `schemars`-roundtripped (or hand-derived if cleaner). Python: `datamodel-code-generator` → pydantic v2. TS: `json-schema-to-typescript`. Java: `jsonschema2pojo`. |
| `docs/api/wire-protocol.md` (MessagePack frames + 13-variant request/response enum) | **Hand-written serde wrappers per language**. Generators don't naturally emit `rmp_serde` externally-tagged enums; pinning the encoding by hand is cheaper than wrestling tooling. The conformance corpus (next bullet) is what catches drift. |
| `docs/api/conformance/` (canonical request/response/event JSON + MessagePack hex blobs) | Every SDK runs the corpus as fixtures. Catches encoding drift before users do. |

**CI gate per SDK:** `make regen-types && git diff --exit-code` — generated code is checked in but must be reproducible. PRs that touch the contract regenerate types as part of the same PR.

## 4. Capabilities

**v1: pure pass-through everywhere.** SDKs treat tokens as opaque base64 strings. No biscuit binding in any v1 SDK. Local verify and local mint/attenuate land in v2 once we know which use cases actually demand them.

## 5. Concurrency model

| Language | Primary | Sync wrapper? |
|---|---|---|
| Rust | `tokio` async | No. |
| Python | `asyncio` | Yes — `CtxdClient` (sync) and `CtxdAsyncClient` (async), like `httpx`. |
| TypeScript/JavaScript | native `Promise` / async iterators | n/a. |
| Java | `CompletableFuture` for v1 | Yes — `CtxdClient` (blocking) and `CtxdAsyncClient` (non-blocking). |

## 6. Ed25519 signature verification

Every SDK ships a `verify_signature(event)` method that recomputes canonical bytes (per `crates/ctxd-core/src/event.rs` rules), looks up the configured public key (passed in at construction or fetched from `GET /health`), and verifies via Ed25519. Library choices:

| SDK | Library |
|---|---|
| Rust | `ed25519-dalek` (already a workspace dep). |
| Python | `cryptography` (mature, packaged everywhere). |
| TypeScript/JavaScript | `@noble/ed25519` (pure-JS, runs in browser + Node, no native deps). |
| Java | JDK 17 built-in `KeyFactory.getInstance("Ed25519")` + `Signature.getInstance("Ed25519")`. No BouncyCastle. |

The canonical-bytes function is identical across SDKs by definition — the conformance corpus (§3) includes a "verify these N events against this public key" fixture so any encoding drift fails CI.

## 7. Per-language design notes

### Rust (`ctxd-client`, crates.io)

Promote the existing `ProtocolClient` (`crates/ctxd-cli/src/protocol.rs:613`). Depends on `ctxd-core` for the Event struct (no copy-paste) and a new `ctxd-wire` crate split out of `ctxd-cli` so the SDK can include the wire types without pulling in axum/rmcp. Public modules: `client`, `events`, `wire`, `errors`. HTTP via `reqwest` (matching ecosystem). MessagePack via `rmp-serde`.

### Python (`ctxd-py`, PyPI)

`httpx` for HTTP (sync + async). `msgpack` for the wire protocol. Generated pydantic v2 types. `cryptography` for Ed25519. `CtxdClient` (sync) and `CtxdAsyncClient` (async). Min Python: 3.10. CI matrix: 3.10, 3.11, 3.12, 3.13. Publish via `uv` + OIDC trusted publishers.

### TypeScript / JavaScript (`@ctxd/client`, npm — single package)

Native `fetch` for HTTP. `@msgpack/msgpack` for wire. `node:net` for TCP, gated by `package.json#exports.browser` so browser builds drop the wire entry. `@noble/ed25519` for verification. CI matrix: Node 20, Node 22, plus a Bun smoke test. Publish with `npm publish --provenance`.

### Java (`ai.sagework.ctxd:ctxd-client`, Maven Central)

`java.net.http.HttpClient` for HTTP, no Apache HttpClient or OkHttp. `msgpack-java` for wire. JDK-built-in Ed25519 (no BouncyCastle). `CtxdClient` (blocking) and `CtxdAsyncClient` (CompletableFuture). Min JDK: 17. Tests run with JUnit 5 + Testcontainers against `ghcr.io/sagework/ctxd:0.3.x`. Publish via Sonatype OSSRH + GPG signing.

## 8. Contract artifact (the v1 deliverable that gates every SDK)

```
docs/api/
  openapi.yaml          # HTTP admin
  wire-protocol.md      # MessagePack frames + request/response enum
  events.schema.json    # CloudEvents + ctxd extensions
  conformance/
    events/             # canonical Event JSON + MessagePack hex blobs
    wire/               # canonical request/response round-trip pairs
    signatures/         # event + pubkey + expected verify result
```

**This lands first.** Every SDK consumes it.

## 9. Versioning & compatibility

- v1 SDK majors track ctxd majors (ctxd 0.3 ↔ SDK 0.3). SDK minors are independent.
- `docs/api/COMPATIBILITY.md` is the matrix; updated on every daemon minor.
- SDKs send `User-Agent: ctxd-py/0.3.4` on HTTP and a `client_version` field on wire `PING` (we extend the verb).
- Daemon's `GET /health` already returns `version`. SDKs warn on mismatch.

## 10. Sequencing

| # | Deliverable | Why |
|---|---|---|
| 0 | `ctxd-wire` crate split (refactor) + `docs/api/` contract folder + conformance corpus | Eat dog food. Validates that the daemon's internals can support an external client. **All SDKs gate on this.** |
| 1 | Rust SDK | Smallest LOC, validates the contract, sets the API shape that other SDKs mirror. |
| 2 | Python SDK | Largest reach. AI/data tooling, scripting, demos. |
| 3 | TypeScript/JavaScript SDK | One package, two runtimes. Real-time browser story deferred. |
| 4 | Java SDK | Last because Maven Central first-publish is the long tail. |

## 11. Effort estimates

Including the type-generation setup (decision #4) and Ed25519 verification in all four (decision #8). One engineer focused, with reviews.

| Deliverable | Days | Notes |
|---|---|---|
| `ctxd-wire` crate split | 2 | Refactor only; no new behavior. |
| `docs/api/` contract + conformance corpus | 3 | OpenAPI + wire-protocol + JSON Schema + ~20 canonical fixtures. |
| Rust SDK | 5.5 | +0.5d for type-gen wiring, no extra for sig verify (already deps). |
| Python SDK | 10 | +1d type-gen, +0d sig verify. |
| TS/JS SDK | 12 | +1d type-gen, +0.5d sig verify (`@noble/ed25519` integration + browser test matrix). |
| Java SDK | 12.5 | +1d type-gen, +0.5d sig verify, +1d Maven Central first-publish. |
| **Total** | **45** | ~9 weeks solo, ~5 weeks two engineers parallel after Rust SDK ships. |

Add 20% buffer for the long tail (Maven publishing, Bun edge cases, generator quirks). Realistic ship window: **2.5–3 months for all four**, fully tested, CI-published, docs-complete.

## 12. Risks

- **Daemon HTTP surface drift in v0.4.** When `dyn Store` migration lands, `/v1/stats` may grow fields. Mitigation: bake additive-only into the API contract — never remove or rename a field within a major.
- **Code-generator quirks.** Generators sometimes emit ugly types or miss edge cases (oneOf, discriminated unions). Mitigation: per-SDK convenience layer hand-written on top of generated low-level types. The generator owns transport+serde; the SDK owns ergonomics.
- **rmp_serde externally-tagged encoding.** Generators won't emit this. Hand-write the wire enum per language; conformance corpus catches drift.
- **Approval-gated write timeouts.** Default to 6 minutes, expose a knob, surface 5xx/timeout as a typed exception.
- **Ed25519 canonical-bytes drift.** Highest-risk cross-SDK invariant. The conformance corpus's signature fixture is the safety net.
- **Maven Central publishing pipeline.** Notorious. Budget the full extra day.

## 13. Repo structure

Monorepo under `clients/` for v1.

```
ctxd/
  clients/
    rust/         # ctxd-client (also part of cargo workspace)
    python/       # ctxd-py
    typescript/   # @ctxd/client
    java/         # ai.sagework.ctxd:ctxd-client
  docs/api/       # contract + conformance
```

Tag-versioned per SDK (`ctxd-client-rs-v0.3.4`, `ctxd-py-v0.3.4`, …). Eventual split is mechanical when one SDK's release cadence diverges from the others.

## 14. First PRs

1. **`refactor: split ctxd-wire crate`** — extract wire-protocol types from `ctxd-cli` into a new `crates/ctxd-wire` so SDK clients depend on it without pulling in axum/rmcp/store deps.
2. **`docs(api): contract artifact + conformance corpus`** — `openapi.yaml`, `wire-protocol.md`, `events.schema.json`, ~20 canonical fixtures.
3. **`feat(http): /v1/peers and DELETE /v1/peers/:id`** OR **`docs: remove /v1/peers from README`** — resolve the documentation discrepancy.
4. **`feat(clients/rust): ctxd-client crate`** — first SDK. Mirrors the API shape every other SDK will copy.

After (4) ships and is published to crates.io, Python and TS work can run in parallel. Java starts last.

## 15. Decisions on record (2026-04-26)

| # | Decision | Choice |
|---|---|---|
| 1 | Primary audience | Application developers |
| 2 | Browser TS story | HTTP-only; defer WebSocket bridge to v2 |
| 3 | Capabilities in v1 | Pass-through everywhere |
| 4 | Types | **Generated from contract artifact** |
| 5 | Sequencing | Rust → Python → TS → Java |
| 6 | Repo structure | Monorepo under `clients/` |
| 7 | Federation primitives in SDK | Never in v1 |
| 8 | Ed25519 signature verification | **In all four SDKs in v1** |
| 9 | Min versions | Python 3.10, Node 20, Java 17, MSRV 1.78 |
| 10 | Docker image | Ship `ghcr.io/sagework/ctxd:0.3.x` as part of SDK milestone |
