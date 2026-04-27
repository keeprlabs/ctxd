# SDK <-> daemon compatibility matrix

This file pins which SDK versions are known-good against which
daemon versions. New rows land here with every breaking change to
the wire protocol, the HTTP admin surface, or the event canonical
form.

| Daemon | ctxd-client-rs | ctxd-py | @ctxd/client | Notes |
| --- | --- | --- | --- | --- |
| 0.3.x  | 0.3.x          | 0.3.x   | 0.3.x        | Initial row. v0.3 introduced the `parents` and `attestation` fields in canonical form; pre-0.3 SDKs cannot verify v0.3 events. |

## How to read this table

A row asserts that any patch release of the SDK in that column
verifies cleanly against any patch release of the daemon in that row.
Minor and major version mismatches are not promised.

## When you add a row

1. Land a daemon release. Cut the SDK release pinned to the same
   commit of `docs/api/`.
2. Run each SDK's conformance corpus against the daemon's
   conformance corpus.
3. Add a row here describing the working pair and any caveats.
4. If the new row makes an old SDK incompatible, leave the old row
   in place — it documents history.
