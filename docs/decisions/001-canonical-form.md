# ADR-001: Canonical Form for Hash Computation

## Context

Events form a hash chain for tamper-evidence. To compute the predecessor hash,
we need a deterministic canonical form for an event. The hash must be stable
across serialization round-trips and must not create circular dependencies.

## Decision

The canonical form:
1. **Excludes** `predecessorhash` and `signature` fields (avoids circular deps)
2. **Includes** all other fields: `specversion`, `id`, `source`, `subject`, `type`, `time`, `datacontenttype`, `data`
3. **Sorts keys alphabetically** using a `BTreeMap` to guarantee deterministic key order
4. **Serializes to JSON bytes** via `serde_json::to_vec`
5. **Hashes with SHA-256** via the `sha2` crate
6. **Outputs as lowercase hex**

## Consequences

- Hash chain is per-subject: each subject path has its own chain. A new event's
  predecessor hash is the hash of the most recent event with the same exact subject.
- Changing the set of included fields or the serialization format is a breaking change.
  It would invalidate all existing hash chains.
- The `BTreeMap` approach is simple but allocates. For v0.1 this is fine; a custom
  streaming canonical serializer could be added for v0.2 if profiling shows it matters.
