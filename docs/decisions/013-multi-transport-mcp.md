# ADR 013: Multi-Transport MCP

- **Status:** Accepted
- **Date:** 2026-04-24
- **Phase:** v0.3 Phase 3D
- **Owners:** ctxd MCP team

## Context

Through v0.2, ctxd exposed its MCP tool surface (`ctx_write`, `ctx_read`,
…) over a single transport: newline-delimited JSON-RPC on stdin/stdout,
the variant Claude Desktop and `mcp-inspector` consume. That works for
local subprocess clients, but it leaves us with three real gaps:

1. **Browser / SaaS clients can't reach a stdio server.** Claude.ai and
   other hosted agents need an HTTP endpoint, and the MCP spec evolved
   in 2025 to define a "streamable HTTP" transport that is now the
   recommended default for non-subprocess clients.
2. **The legacy SSE transport is still entrenched.** A long tail of
   custom agents speaks the older `GET /sse + POST /messages` pair. We
   want to support those without forcing a rewrite.
3. **Auth on stdio was an afterthought.** The token was a tool argument
   — fine for an experiment, awkward for a real deployment where
   credentials should travel out-of-band.

The goal of this phase is to serve the same `CtxdMcpServer` over **all
three** transports concurrently, with a single auth model.

## Decision

We support three transports out of the same daemon process:

| Transport | Module | Endpoint | When to use |
| --- | --- | --- | --- |
| **stdio** | `ctxd_mcp::transport::run_stdio` | stdin/stdout | Claude Desktop, mcp-inspector, local subprocess clients. |
| **SSE (legacy)** | `ctxd_mcp::transport::run_sse` | `GET /sse` + `POST /messages` | Custom agent frameworks still on the pre-2025-03-26 transport. |
| **streamable HTTP** | `ctxd_mcp::transport::run_streamable_http` | `POST /mcp` (JSON or SSE) | Modern hosted clients (Claude.ai, agent frameworks targeting MCP 2025-03-26+). |

The CLI exposes one flag per transport on `ctxd serve`:
`--mcp-stdio` (default on), `--mcp-sse <addr>`, `--mcp-http <addr>`.
Each enabled transport runs in its own tokio task; failures are logged
but do not crash siblings (a stdio EOF only ends the stdio task — SSE
and streamable-HTTP keep serving, and vice versa).

### Auth precedence

Capability tokens are accepted via two channels:

1. `Authorization: Bearer <base64-biscuit>` HTTP header (HTTP transports
   only).
2. Per-tool-call `token` argument (the v0.1 stdio convention; preserved
   for parity).

**When both are present, the header wins.** This is enforced in a
single place — the axum middleware in
`ctxd_mcp::transport::auth_middleware` — by rewriting
`params.arguments.token` in the JSON-RPC body before the request
reaches rmcp. The tool handlers themselves are unchanged: they still
receive a single optional `token`, and the cap engine logic on the
back end is identical to what stdio has used since v0.1.

A new `--require-auth` flag enables a stricter mode: every
`tools/call` over the HTTP transports must present a token (header or
arg); calls without one return 401 Unauthorized. Stdio is unaffected
— it keeps the legacy "open by default" behaviour because stdio
clients are local subprocesses owned by the same user.

### Feature gate: `http-transports`

The streamable-HTTP and SSE transports add a sizable transitive graph
(axum, hyper, tower, rmcp's `transport-streamable-http-server`
feature). Library consumers that embed `ctxd-mcp` purely for stdio
should not pay that compile cost.

We gate both HTTP transports behind a non-default Cargo feature:

```toml
[dependencies]
ctxd-mcp = { path = "../ctxd-mcp" }              # stdio only (default)
ctxd-mcp = { path = "../ctxd-mcp", features = ["http-transports"] }
```

`ctxd-cli` enables `http-transports` so the binary always has all
three. Library callers — including future ctxd embedders that just
want a local agent process — pay zero axum cost.

### TLS

**v0.3 does not ship TLS in-process.** Production users must front the
HTTP listeners with a reverse proxy (nginx, Caddy, AWS ALB, …) that
terminates TLS and forwards plaintext HTTP to the daemon.

Rationale:

* TLS configuration is a deployment concern, not an application
  concern. Every team that runs us at scale already has a TLS-capable
  edge.
* Maintaining a TLS stack inside the daemon means tracking certificate
  rotation, ACME, OCSP, cipher policy, etc. — none of which is core
  ctxd value.
* The reverse-proxy pattern is what the MCP working group's hosted
  clients (Claude.ai, AnthropicWS) expect. Direct TLS would only help
  the rare on-prem-no-proxy deployment.

If we eventually need in-process TLS we'll revisit this — the auth
layer is independent of the transport, so adding rustls would be
purely additive.

## Alternatives considered

### A) Stay stdio-only, defer HTTP

Cleanest for the codebase but blocks every non-subprocess client. SSE
clients, browser-resident agents, and Claude.ai integrations all
require an HTTP endpoint. Rejected.

### B) Streamable HTTP only (drop legacy SSE)

The MCP spec deprecated the SSE transport in March 2025 in favour of
streamable HTTP. We could ship only the new transport and tell SSE
clients to upgrade. Rejected because a meaningful chunk of agent
frameworks haven't migrated, and the SSE layer is small (~200 LOC of
axum routing + an mpsc-backed `Transport` impl) — cheap to maintain
for the next year while the ecosystem catches up.

### C) Roll our own JSON-RPC framing for HTTP

We could write a transport from scratch on top of axum, leaving rmcp
out of the HTTP path. Rejected because rmcp 1.5 ships
`StreamableHttpService` exactly for this use case, with session
management, DNS rebinding protection, and the SSE-or-JSON content
negotiation already implemented. Reusing it removed ~500 LOC and any
need to track the spec's revisions ourselves.

### D) Auth via tool arg only

Keep the v0.1 token-in-body model for HTTP too. Rejected because (a)
mainstream HTTP tooling (curl, reqwest, browsers, reverse proxies)
already understands `Authorization: Bearer …`; (b) the per-call arg
makes it impossible to enforce auth at the edge without parsing every
JSON-RPC body; (c) header-based auth is what every hosted MCP client
will reach for first.

### E) Auth via tool arg only inside the daemon (HTTP just forwards)

Have the middleware parse the header but not modify the body — let
the tool handler read both `extensions` and `params.token` and decide.
Rejected because it spreads the precedence rule across nine call
sites (eight tool methods + the middleware). The body-rewrite pattern
keeps the policy in one file.

## Consequences

### Positive

* The same daemon now serves Claude Desktop (stdio), legacy agents
  (SSE), and modern hosted agents (streamable HTTP) without
  duplication.
* Header-based auth means deploying behind a reverse proxy is trivial
  — the proxy can short-circuit unauthenticated requests entirely.
* The `http-transports` feature gate keeps stdio-only embedders lean.
* DoS defence: 1 MiB JSON-RPC body limit on all HTTP transports.

### Negative / open

* The SSE transport is hand-rolled. If rmcp ever ships a first-party
  legacy-SSE server, we should switch and delete the in-tree version.
  Tracked as a TODO; the surface is small (single file, ~200 LOC).
* Session management on streamable HTTP is **stateless** today
  (one rmcp service per request). Stateful mode (`Mcp-Session-Id`
  header threading) is supported by `StreamableHttpService` but we
  default off — sessions add memory cost we don't yet need. If a
  future client requires it, flip the config flag.
* The middleware re-buffers POST bodies up to 1 MiB. For typical tool
  calls that's negligible; for a hypothetical "upload 10 MB of context
  in one shot" tool we'd need to revisit.
* No TLS in-process — operators must configure a reverse proxy for
  any non-loopback deployment. Documented in `docs/mcp.md`.

## When to revisit

* When rmcp 1.x ships a server-side SSE transport, swap our in-tree
  SSE for theirs.
* When a production deployment needs in-process TLS (e.g. an air-gapped
  install where the operator can't run nginx), revisit the TLS decision
  and add rustls behind a feature flag.
* When the per-call body exceeds 1 MiB on a real workload, raise the
  limit (configurable today via `AuthMiddlewareConfig::max_body_bytes`
  if a downstream caller wants to override).
