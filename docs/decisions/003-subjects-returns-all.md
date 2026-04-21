# ADR-003: ctx.subjects Returns All Levels

## Context

The `ctx.subjects` MCP tool lists subjects in the store. The spec left ambiguous
whether it returns one level (like `ls`) or recurses (like `find`).

## Decision

`ctx.subjects` with `recursive=false` returns only exact matches at the given prefix.
`ctx.subjects` with `recursive=true` returns the prefix and all descendants.
Without a prefix, it returns all distinct subjects in the store.

This matches the behavior of `ctx.read` for consistency.

## Consequences

- Agents get a full directory listing with one call when using recursive mode.
- For stores with many subjects, this could return a large result. Pagination is a v0.2 concern.
