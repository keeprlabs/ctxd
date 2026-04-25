# ADR 007 — TEE attestation passthrough

## Status

Accepted, v0.3. Actual TEE SDK integration (AMD SEV-SNP, Intel TDX,
Nitro Enclaves, ARM CCA) is explicitly v0.4.

## Context

Adapters running in hardware-attested execution environments produce
attestation payloads that prove the code that generated an event ran
inside a specific enclave. Downstream consumers — especially federated
peers — may want to verify these attestations before accepting an
event.

v0.3 needs to carry the attestation byte-stream without interpreting
it, so the infrastructure is in place when v0.4 plugs in concrete
TEE SDKs.

## Decision

- `Event.attestation: Option<Vec<u8>>` (added in Phase 1B) carries raw
  bytes produced by an adapter. Canonical form includes
  `attestation` unconditionally (hex-encoded string when present,
  JSON `null` otherwise) so hashes and signatures are stable across
  peers regardless of attestation presence.
- Replication streams the bytes through **unchanged** — peers do not
  re-attest events they receive; the origin's attestation is what
  matters.
- `CapEngine::verify` gains an optional
  `attestation_verifier: Option<Box<dyn Fn(&[u8]) -> bool + Send + Sync>>`
  hook. When `None` (the v0.3 default), any `attestation` value —
  including absence — is accepted. When `Some(f)`, the verifier is
  invoked against the event's attestation bytes and the event is
  rejected if `f` returns `false`.
- No TEE SDK is linked into the default build. v0.4 will ship
  opt-in crate features (`tee-sev-snp`, `tee-tdx`, etc.) that
  construct verifier closures.

## Rationale

- **Forward compatibility**: the on-disk + on-wire format is stable
  now. v0.4 cannot silently change the hash of existing events.
- **Zero-cost when off**: no TEE verification overhead when no
  attestation is present or no verifier is installed.
- **Replication-safe**: federated peers agree on event identity
  regardless of whether they can locally verify attestations.

## Consequences

- `Event` (and therefore every on-wire representation) is ~32B larger
  per event on average — the `attestation: null` field in canonical
  JSON costs bytes.
- Adapters that don't produce attestations simply leave the field as
  `None`; there's no API break for v0.2-style adapters.
- The `attestation_verifier` hook is only wired into `CapEngine::verify`
  in v0.4; until then, the field is purely data-plane.

## Revisit

When v0.4 ships TEE SDK integrations, revisit whether `attestation`
should be a structured type instead of an opaque `Vec<u8>` — the raw
bytes approach trades downstream ergonomics for format independence.
