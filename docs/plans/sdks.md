# Client SDK Plan

**Status:** approved by Mani 2026-04-26. v1 ships **three SDKs**: Rust, Python, TypeScript/JavaScript. Java is deferred — pulling it out removes the only human-blocked item (Sonatype Maven Central namespace claim) and lets the entire SDK milestone run on agent + token-based publishers.

Decisions on record:

| # | Decision | Choice |
|---|---|---|
| 1 | Primary audience | Application developers writing services that ingest into / query ctxd |
| 2 | Browser TS story | HTTP-only; defer WebSocket bridge to v2 |
| 3 | Capabilities in v1 | Pass-through tokens (no biscuit binding in any SDK) |
| 4 | Types | Generated from a contract artifact under `docs/api/` |
| 5 | Sequencing | Rust → Python → TS/JS |
| 6 | Repo structure | Monorepo under `clients/` |
| 7 | Federation primitives in SDK | Never in v1 |
| 8 | Ed25519 signature verification | In all three SDKs in v1 |
| 9 | Min versions | Python 3.10, Node 20, MSRV 1.78 |
| 10 | Docker image | `ghcr.io/keeprlabs/ctxd:0.3.x`, published via `GITHUB_TOKEN` from CI |
| 11 | Java | **Deferred to a later milestone** — see §11 |

## 1. Goals & non-goals

### Goals

Ship official client SDKs in **Rust, Python, and TypeScript/JavaScript** that let application developers talk to a `ctxd serve` instance without writing transport plumbing. Primary audience: services that ingest into ctxd or query it. Adapter authors and admin / observability tooling fall out of the same API.

### Non-goals (v1)

- Not reimplementing the daemon.
- Not shipping federation peer behavior. Wire-protocol federation verbs (`PEER_HELLO`, `PEER_REPLICATE`, …) are daemon-only by policy.
- Not shipping an MCP server or client. Use `rmcp` / `@modelcontextprotocol/sdk` and friends; point them at `ctxd serve --mcp-http`.
- Not shipping TLS termination. Per ADR 013, production runs behind a reverse proxy.

## 2. API surface

| Transport | Rust | Python | TS/JS | Notes |
|---|---|---|---|---|
| HTTP admin (`/v1/*`) | ✅ | ✅ | ✅ | Universal. |
| Wire protocol (TCP+MsgPack) | ✅ | ✅ | Node-only | Browser TCP impossible; browser gets HTTP only. |
| Real-time `SUB` | ✅ | ✅ | Node ✅ / browser polling | Browser polls via HTTP `since` semantics. WS bridge deferred to v2. |
| MCP transports | — | — | — | Out of scope. |
| Federation verbs | — | — | — | Out of scope, never in v1. |
| Ed25519 sig verify | ✅ | ✅ | ✅ | All three SDKs. |

**Approval-gated writes** can block up to 5 minutes (`docs/mcp.md`). Every SDK's HTTP write path defaults to a 6-minute timeout with a knob to override.

**Documentation discrepancy to resolve before shipping:** the v0.3 README and CHANGELOG mention `GET /v1/peers` and `DELETE /v1/peers/:id`, but those endpoints do not exist in `crates/ctxd-http/src/router.rs`. Either add them to the daemon as part of the SDK milestone, or correct the docs. Decision before publishing the OpenAPI spec.

## 3. Types: generated from the API contract

The contract artifact in `docs/api/` is the source of truth. SDK types are **generated** from it on every CI run; drift between SDK and spec fails CI.

| Artifact | Generators per language |
|---|---|
| `docs/api/openapi.yaml` (HTTP admin) | Rust: `progenitor`. Python: `openapi-python-client`. TS: `openapi-typescript` + `openapi-fetch`. |
| `docs/api/events.schema.json` (CloudEvents + ctxd extensions) | Rust: `schemars`-roundtripped (or hand-derived if cleaner). Python: `datamodel-code-generator` → pydantic v2. TS: `json-schema-to-typescript`. |
| `docs/api/wire-protocol.md` (MessagePack frames + 13-variant request/response enum) | **Hand-written serde wrappers per language.** Generators don't naturally emit `rmp_serde` externally-tagged enums; pinning the encoding by hand is cheaper than wrestling tooling. The conformance corpus is what catches drift. |
| `docs/api/conformance/` (canonical request/response/event JSON + MessagePack hex blobs) | Every SDK runs the corpus as fixtures. Catches encoding drift before users do. |

**CI gate per SDK:** `make regen-types && git diff --exit-code` — generated code is checked in but must be reproducible. PRs that touch the contract regenerate types in the same PR.

## 4. Capabilities

**v1: pure pass-through everywhere.** SDKs treat tokens as opaque base64 strings. No biscuit binding in any v1 SDK. Local verify and local mint/attenuate land in v2 once we know which use cases actually demand them.

## 5. Concurrency model

| Language | Primary | Sync wrapper? |
|---|---|---|
| Rust | `tokio` async | No. |
| Python | `asyncio` | Yes — `CtxdClient` (sync) and `CtxdAsyncClient` (async), like `httpx`. |
| TypeScript/JavaScript | native `Promise` / async iterators | n/a. |

## 6. Ed25519 signature verification

Every SDK ships a `verify_signature(event)` method that recomputes canonical bytes (per `crates/ctxd-core/src/event.rs` rules), looks up the configured public key (passed in at construction or fetched from `GET /health`), and verifies via Ed25519.

| SDK | Library |
|---|---|
| Rust | `ed25519-dalek` (already a workspace dep). |
| Python | `cryptography` (mature, packaged everywhere). |
| TypeScript/JavaScript | `@noble/ed25519` (pure-JS, runs in browser + Node, no native deps). |

The conformance corpus (§3) includes a "verify these N events against this public key" fixture so any encoding drift fails CI in every SDK.

## 7. Per-language design notes

### Rust (`ctxd-client`, crates.io)

Promote the existing `ProtocolClient` (`crates/ctxd-cli/src/protocol.rs`). Depends on `ctxd-core` for the Event struct (no copy-paste) and a new `ctxd-wire` crate split out of `ctxd-cli` so the SDK can include the wire types without pulling in axum/rmcp. Public modules: `client`, `events`, `wire`, `errors`. HTTP via `reqwest`. MessagePack via `rmp-serde`. Publish via crates.io, OIDC trusted publisher from CI.

### Python (`ctxd-py`, PyPI)

`httpx` for HTTP (sync + async). `msgpack` for the wire protocol. Generated pydantic v2 types. `cryptography` for Ed25519. `CtxdClient` (sync) and `CtxdAsyncClient` (async). Min Python 3.10. CI matrix: 3.10, 3.11, 3.12, 3.13. Publish via PyPI trusted publishers (OIDC) from GitHub Actions.

### TypeScript / JavaScript (`@ctxd/client`, npm — single package)

Native `fetch` for HTTP. `@msgpack/msgpack` for wire. `node:net` for TCP, gated by `package.json#exports.browser` so browser builds drop the wire entry. `@noble/ed25519` for verification. CI matrix: Node 20, Node 22, plus a Bun smoke test. Publish with `npm publish --provenance` via a GitHub Actions OIDC token.

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
| 0 | `ctxd-wire` crate split + `docs/api/` contract folder + conformance corpus | Eat dog food. Validates that the daemon's internals can support an external client. **All SDKs gate on this.** |
| 1 | Rust SDK | Smallest, validates the contract, sets the API shape. |
| 2 | Python SDK | Largest reach. AI/data tooling, scripting, demos. |
| 3 | TypeScript/JavaScript SDK | One package, two runtimes (Node + browser). |

After (1) ships, (2) and (3) can run in parallel.

## 11. Java — deferred and why

Java was in the original draft. Pulling it out because:

- **Maven Central namespace claim is human-blocked.** Sonatype OSSRH first-time approval takes hours-to-days; a verified namespace under our own domain requires DNS verification. That's calendar time, not engineering time, and there is no reliable workaround that keeps the package on Maven Central.
- **GitHub Packages Maven works around the above** but it's a worse end-user experience (every consumer adds a custom repository to their `pom.xml`/`build.gradle`). For a public OSS SDK, that's a non-trivial friction tax.
- **No JVM users in our active feedback loop yet.** Without a concrete user asking, shipping a Java SDK before Rust + Python + TS is likely to ship into a void.

When we revisit:

- A user shows up with a JVM use case, OR
- We complete the Sonatype OSSRH + namespace claim in the background (calendar time only, no engineering blocker), at which point Java drops in as ~2 agent runs once the publishing pipeline is unblocked.

## 12. Effort estimates (agent runs)

Calibrated against v0.3's actual track record: each phase ran ~30–60 min wall-clock per agent, with stalls and merge passes adding ~20 min cleanup per round.

| Deliverable | Agent runs | Wall-clock | Notes |
|---|---|---|---|
| `ctxd-wire` crate split | 1 | ~30 min | Refactor only. Tight scope. |
| `docs/api/` contract + conformance corpus | 1 | ~60 min | Hand-write spec + run a fixture-capture script against a live daemon. |
| Rust SDK (`ctxd-client`) | 1 | ~45 min | Promotes existing `ProtocolClient`; pulls in generated OpenAPI client + JSON Schema types. |
| Python SDK (`ctxd-py`) | 1–2 | ~60–90 min | Sync + async wrappers; pydantic v2 from schema; test matrix on 3.10–3.13. May need a follow-up if `openapi-python-client` produces ugly types. |
| TS/JS SDK (`@ctxd/client`) | 2 | ~90–120 min | Browser/Node split; Bun smoke test; `@noble/ed25519` browser test. |
| Ed25519 cross-SDK conformance fixtures | 1 | ~30 min | All three SDKs verify the same fixture; if any drifts, that agent fixes it. |
| Merge / cleanup / CI fixes | — | ~60 min total | v0.3 took ~3 CI round-trips; plan for similar. |
| **Total** | **7–9 agent runs** | **~6–9 hours wall-clock** | Sequential with supervision. |

### What's no longer human-blocked

With Java dropped:

- **crates.io** — `CARGO_REGISTRY_TOKEN` from a one-time CI secret. No human approval.
- **PyPI** — trusted publishers via OIDC (a few clicks on PyPI to set up; no human approval after that).
- **npm** — `NPM_TOKEN` or OIDC; `--provenance` flag bakes in supply-chain attestation.
- **ghcr.io Docker image** — `GITHUB_TOKEN` is automatically available in CI.

Every publish path runs from GitHub Actions with no human in the loop after initial token / trusted-publisher setup.

## 13. Risks

- **Daemon HTTP surface drift in v0.4.** When `dyn Store` migration lands, `/v1/stats` may grow fields. Mitigation: bake additive-only into the API contract — never remove or rename a field within a major.
- **Code-generator quirks.** Generators sometimes emit ugly types or miss edge cases (oneOf, discriminated unions). Mitigation: per-SDK convenience layer hand-written on top of generated low-level types. The generator owns transport+serde; the SDK owns ergonomics.
- **rmp_serde externally-tagged encoding.** Generators won't emit this. Hand-write the wire enum per language; conformance corpus catches drift.
- **Ed25519 canonical-bytes drift.** Highest-risk cross-SDK invariant. The conformance corpus's signature fixture is the safety net.
- **Approval-gated write timeouts.** Default to 6 minutes, expose a knob, surface 5xx/timeout as a typed exception.
- **Stalls.** v0.3 had two stalls on the 600s watchdog. Plan for 1–2 stalls; each costs ~10 min to relaunch with resumption notes.

## 14. Repo structure

Monorepo under `clients/`.

```
ctxd/
  clients/
    rust/         # ctxd-client (also part of cargo workspace)
    python/       # ctxd-py
    typescript/   # @ctxd/client
  docs/api/       # contract + conformance
```

Tag-versioned per SDK (`ctxd-client-rs-v0.3.4`, `ctxd-py-v0.3.4`, …). Eventual split into per-SDK repos is mechanical when one SDK's release cadence diverges from the others.

## 15. First PRs

1. **`refactor: split ctxd-wire crate`** — extract wire-protocol types from `ctxd-cli` into a new `crates/ctxd-wire` so SDK clients depend on it without pulling in axum/rmcp/store deps.
2. **`docs(api): contract artifact + conformance corpus`** — `openapi.yaml`, `wire-protocol.md`, `events.schema.json`, ~20 canonical fixtures.
3. **Resolve the `/v1/peers` discrepancy** — README claims it; `router.rs` doesn't have it. Either add the endpoint or correct the docs.
4. **`feat(clients/rust): ctxd-client crate`** — first SDK. Mirrors the API shape every other SDK will copy.

After (4), Python and TS work runs in parallel.
