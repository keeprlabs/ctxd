# ADR-002: Capability Verification Model

## Context

Biscuit tokens store facts in their authority block. The biscuit datalog language
has limited string operations, so complex glob matching (like `/test/**` matching
`/test/a/b/c`) cannot be expressed purely in datalog policies.

## Decision

Verification uses a two-pass approach:

1. **First pass (biscuit-native):** Try exact subject match and `/**` wildcard match
   via biscuit authorizer policies. This handles the common cases efficiently.

2. **Second pass (Rust fallback):** If the first pass fails, extract all `right(pattern, op)`
   facts from the token via a datalog query, then perform glob matching in Rust code.
   If a match is found, re-authorize with the matched right injected as a fact to ensure
   block checks (expiry, attenuation) still apply.

## Consequences

- Glob matching is correct for all patterns, not limited by datalog string ops.
- The two-pass approach adds ~1 authorizer creation for glob patterns, which is negligible.
- Attenuation checks use `starts_with` in datalog, which approximates glob scope.
  This is slightly more permissive than exact glob matching but acceptable for v0.1.
