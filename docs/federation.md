# Federation

ctxd federation lets two or more daemons replicate events bidirectionally
while preserving the per-peer hash chain and the cross-peer signature
chain. Federation is **opt-in per peer**: there is no service discovery,
no consensus, and no central registry — every peer pair is the result of
an explicit `peer add` (or its handshake-driven equivalent).

This document covers the concept, a copy-paste two-node tutorial, and a
troubleshooting section for the most common failure modes.

## Concept

A federated topology in ctxd is a set of *peers* connected by directed
trust edges. Each peer is identified by an Ed25519 public key (32 bytes,
hex-encoded as the `peer_id`). Each edge carries:

- a **subject grant glob** ("you may send me events under `/work/**`"),
- a **biscuit capability token** the granter mints for the grantee, and
- a **replication cursor** so resume after disconnect is exactly-once.

Three rules cover the steady state:

1. **Every replicated event is signed.** Inbound `PeerReplicate`
   messages whose payload signature does not verify against the peer's
   stored pubkey are rejected before `Store::append`. No exceptions.
2. **Every replicated event must fit the granted scope.** If the
   subject is outside the receiver's `we_grant_remote` glob list, the
   event is rejected. No silent ignores.
3. **Replication makes one lap, then stops.** The
   `PeerReplicate.origin_peer_id` envelope is preserved on rebroadcast
   (see [ADR 009](decisions/009-federation-loop-guard.md)), so a
   three-node ring fans the event out exactly once.

Cursors and parent backfill follow the rules in
[ADR 010](decisions/010-cursor-resume-and-parent-backfill.md). The
KV view's last-writer-wins rule is in
[ADR 006](decisions/006-federation-lww.md). Automatic capability
exchange on `peer add` is documented in
[ADR 008](decisions/008-automatic-capability-exchange.md).

## Two-node tutorial

Spin up two daemons on a single host using two SQLite files and two
ports. Open three terminals.

```bash
# Terminal 1 — Alice
export CTXD_FEDERATION_AUTO_ACCEPT=true   # accept any inbound peer
ctxd --db /tmp/alice.db serve \
  --bind 127.0.0.1:7777 \
  --wire-bind 127.0.0.1:7778 \
  --no-mcp-stdio

# Terminal 2 — Bob
export CTXD_FEDERATION_AUTO_ACCEPT=true
ctxd --db /tmp/bob.db serve \
  --bind 127.0.0.1:8777 \
  --wire-bind 127.0.0.1:8778 \
  --no-mcp-stdio
```

(Use `--mcp-stdio false` if your build of ctxd doesn't accept the
no- prefix; the daemons just need to expose `--wire-bind`.)

In a third terminal, perform the handshake. The peer ids printed by
each daemon's startup log are the pubkey hex; copy each into the
opposite daemon's `peer add` invocation.

```bash
# Alice → Bob
ctxd --db /tmp/alice.db peer add \
  --peer-id <bob-pubkey-hex> \
  --url 127.0.0.1:8778 \
  --subjects "/work/**"

# Bob → Alice (so replication is bidirectional)
ctxd --db /tmp/bob.db peer add \
  --peer-id <alice-pubkey-hex> \
  --url 127.0.0.1:7778 \
  --subjects "/work/**"
```

Each `peer add` opens a TCP connection, sends `PeerHello`, awaits
`PeerWelcome`, persists the cap in `peers` table, and returns. Both
daemons now see the other in `peer list`.

Now write something on Alice and read it on Bob:

```bash
ctxd --db /tmp/alice.db connect --addr 127.0.0.1:7778 \
  pub --subject /work/note/hello --type ctx.note --data '{"msg":"hi from alice"}'

# Wait ~250ms for replication, then:
ctxd --db /tmp/bob.db read --subject /work/note --recursive
```

You should see Alice's event in Bob's log with byte-identical `id`,
`predecessorhash`, `signature`, and `parents`.

## Auto-accept policy

The `CTXD_FEDERATION_AUTO_ACCEPT` env var on the **receiver** controls
whether an inbound `PeerHello` is honored:

- unset or `false` (default) — every inbound handshake is rejected.
- `true` — every inbound handshake is accepted.
- `allowlist:<hex1>,<hex2>,...` — accept only the listed pubkeys.

Production deployments should use `allowlist:` mode. `true` is for
demos and CI.

## Troubleshooting

**Symptom — `peer add --url …` returns "handshake failed: connection
refused".** Confirm the remote daemon is running and `--wire-bind`'d on
the URL you supplied. The wire protocol is plain MessagePack-over-TCP;
it doesn't share the HTTP `--bind` port.

**Symptom — `peer add` returns "auto-accept policy denied peer …".**
The receiver has `CTXD_FEDERATION_AUTO_ACCEPT=false` (or unset).
Either set it to `true` or add the caller's pubkey to its allowlist
and restart the receiver.

**Symptom — events don't replicate after `peer add`.** Ensure both
sides ran `peer add` against each other — replication is unidirectional
on a single edge. If `alice peer add bob` happens but `bob peer add
alice` does not, Alice can't dial Bob back (her record of Bob is an
inbound stub with `url: inbound:<pk>` and `send_replicate` skips those).

**Symptom — `cap scope violation` on every replicate.** The receiver's
`peer.granted_subjects` does not include the subject the sender used.
Run `ctxd peer grant --peer-id <id> --subjects "/work/**,/notes/**"`
on the receiver to widen the grant.

**Symptom — KV value differs between peers.** Cursors converge to
the higher `(time, event_id)` under
[ADR 006](decisions/006-federation-lww.md). If they diverge, you've
likely hit a clock-skew window > the inter-event spacing. The
**event log** still agrees (both branches are visible via
`ctxd read --recursive`); only the materialized KV view picks a
winner. If your peers' clocks drift by > 1s consistently, the LWW
window is too tight — open an issue.

**Symptom — `backfill stalled` warning in the log.** A child event
referenced parents the origin peer doesn't have either. Confirm the
origin peer's store contains the parents:
`ctxd --db <origin> read --subject / --recursive | grep <parent-id>`.
If not, the chain is broken upstream and you'll need to repair the
origin's store before replication can complete.
