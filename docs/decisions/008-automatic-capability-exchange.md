# ADR 008 — Automatic capability exchange on `peer add`

## Status

Accepted, v0.3 Phase 2B.

## Context

In v0.2 `peer add` was a database-only operation: it wrote a
`(peer_id, url, public_key, granted_subjects)` row into the `peers`
table and stopped. Federation was opaque to the operator — there was
no signal that the remote was reachable, no exchange of capability
tokens, and no way to know whether the remote would accept events
matching the local-side grant.

For v0.3 we want federation to be a one-line operator step. The
operator runs `peer add --url <addr> --subjects <globs>` and gets
back a structured success or a clear error. To do that, `peer add`
needs to actually talk to the remote.

The biscuit cap engine supports minting attenuated tokens, so the
ingredients for a real handshake (each side mints a cap for the
other) are in place. The wire protocol's federation variants
(`PeerHello`, `PeerWelcome`) were stubbed in v0.3 Phase 2A.

## Decision

`peer add --url <addr>` performs a real two-step handshake by default:

1. **Hello (sender side).** Mint `cap_local_for_remote` over the
   subject globs the operator passed via `--subjects`, with operations
   `[Read, Subscribe, Peer]`. Open a TCP connection to `<addr>`. Send
   `PeerHello { peer_id: local_pubkey_hex, public_key: local_pubkey,
   offered_cap: <base64>, subjects: <globs> }`.

2. **Welcome (receiver side).** Apply `auto_accept` policy. If
   accepted: mint `cap_remote_for_local` matching the same scope
   (mirror of what the sender granted); persist the inbound peer in
   the `peers` table; respond with `PeerWelcome { peer_id, public_key,
   offered_cap, subjects }`. If denied: respond `Error { message: "..." }`.

3. **Persist (both sides).** Each side records pubkey + URL + grants
   + cap into its own `peers` table.

The auto-accept policy is read from
`CTXD_FEDERATION_AUTO_ACCEPT={true|false|allowlist:<hex1>,<hex2>,...}`.
Default is `false`.

A `--manual` flag preserves the v0.2 behavior (record only, no
handshake). It exists for offline enrollment and test fixtures.

## Rationale

- **Operator ergonomics.** A single CLI command must give a definitive
  answer about whether replication will work. Skipping the dial
  meant operators had to verify connectivity by side effects.
- **Symmetric exchange.** Each side mints the cap it issues, so each
  side's root key remains the source of authority for events it
  receives. Verifying inbound replication only requires one trusted
  pubkey: the remote's.
- **Env-driven policy beats config files.** v0.3 doesn't ship a
  config file. The env knob keeps deployment artifacts simple — `1`
  Helm value, no on-disk state to keep in sync.

## Consequences

- `peer add --url <addr>` requires the receiver to be running, which is
  fine in normal operations but a regression for offline workflows.
  The `--manual` escape hatch covers that.
- Either side can re-run `peer add` to refresh the granted subjects
  without breaking replication; the cursor is keyed on `(peer_id,
  subject_pattern)` and survives grant changes.
- An operator MUST set `CTXD_FEDERATION_AUTO_ACCEPT` on the receiver
  for the handshake to succeed. The default `false` is the safe
  default — it surfaces a clear error message if missed.

## Revisit

If we ship a config file in v0.4, fold the auto-accept policy into it
and keep the env var as an override. We also expect to add a TLS-or-
better transport layer (currently the wire protocol is plain TCP);
when that happens, the handshake should pin the remote pubkey to the
TLS leaf certificate.
