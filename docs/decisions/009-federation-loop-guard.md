# ADR 009 — Federation loop guard

## Status

Accepted, v0.3 Phase 2D.

## Context

Replication in ctxd happens via the `BroadcastEvent` channel: every
PUB locally and every successful inbound `PeerReplicate` re-fans-out
the event so the next hop in a multi-peer topology can pick it up.
Without an explicit guard, an event in a closed topology (a ring
A → B → C → A, or any cycle) would replicate forever:

```text
A pub → B receives → B re-broadcasts → C receives → C re-broadcasts
      → A receives → A re-broadcasts → B receives → ... ∞
```

`Store::append` is idempotent on event id (UNIQUE constraint on
`events.id`), so the *append* is a no-op on the second arrival, but
the *re-broadcast* keeps firing. The wire-level chatter is enough to
saturate a small cluster.

We need a deterministic rule to stop replication after exactly one
lap.

## Decision

Each `PeerReplicate` request carries an `origin_peer_id` envelope
field. The local `BroadcastEvent` carries the same field. The rule:

> **A daemon does not send an event back to its origin peer.**

Mechanically:

1. **Local PUB.** `handle_pub` emits `BroadcastEvent { ...,
   origin_peer_id: "" }`. The federation broadcast subscriber
   interprets empty as "produced locally" and uses the local peer-id
   as the origin.
2. **Inbound replicate.** `handle_peer_replicate` re-emits the event
   to the local `BroadcastEvent` with the *upstream* origin
   preserved, not the local id.
3. **Outbound fan-out.** For every enrolled peer, the subscriber
   compares `origin == peer.peer_id`. If they match, drop. Otherwise
   forward.

`origin_peer_id` is **not** persisted on the event itself. It's a
transport-level envelope — the canonical form (which the signature
binds) is invariant across hops.

## Rationale

- **Stateless.** The receiver doesn't need to maintain per-event
  visited sets; the origin tag is one string and the test is one
  comparison.
- **Doesn't bake topology into the event.** Persisting origin would
  bake "who first published this" into history forever, and rotating
  pubkeys would break the chain.
- **Trust assumption is bounded.** A peer that forges
  `origin_peer_id = X` to push events into our store still has to
  produce a signature that verifies against X's pubkey. Without that,
  `verify_inbound` rejects the event before any broadcast happens.
  The loop guard is a *deduplication* control, not a security
  control.

## Consequences

- A non-malicious mesh of any topology converges in one fan-out lap.
- A malicious peer that *can* forge origin_peer_id can still trigger
  one extra outbound to the spoofed peer (the "victim" of the spoof
  receives an event from us that it would have refused from us
  anyway, because the signature won't verify against our pubkey on
  their side). The cost is one wasted outbound; the security
  invariant — events stored only on signature-valid peers — is
  preserved.
- A peer that intentionally re-publishes an old event (replays
  yesterday's news today, same id) gets through to neighbours but
  stops there because of UNIQUE on `events.id`. No-op on the receive
  side, which is the desired behaviour.

## Revisit

If we add multi-tenant federation in v0.4 where one peer-id covers
multiple subject namespaces, the rule may need to be `(origin_peer_id,
subject_root)`-keyed. Today, peers are global so the simple key works.
