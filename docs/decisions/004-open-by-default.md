# ADR-004: Open by Default (No Token = Allowed)

## Context

MCP tools accept an optional capability token. The question is what happens
when no token is provided.

## Decision

For v0.1, if no capability token is provided in an MCP tool call, the operation
is allowed. This is "open by default" for local development.

## Consequences

- Zero-config local development: agents can use ctxd immediately without minting tokens.
- Not suitable for multi-tenant or production deployments without change.
- v0.2 should add a `--require-auth` flag to the daemon that rejects unauthenticated requests.
