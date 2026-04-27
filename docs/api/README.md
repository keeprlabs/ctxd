# ctxd API contract

This directory is the **source of truth** for every ctxd SDK. The
three first-party SDKs (`ctxd-client-rs`, `ctxd-py`, `@ctxd/client`)
generate types and conformance tests from these files. If you change
something here, every SDK either picks the change up automatically or
fails its conformance tests — that is the design.

## Layout

```
docs/api/
  README.md             # this file
  openapi.yaml          # OpenAPI 3.1 spec for the HTTP admin surface
  wire-protocol.md      # MessagePack wire protocol (publish/subscribe/query)
  events.schema.json    # JSON Schema 2020-12 for the Event payload
  COMPATIBILITY.md      # SDK <-> daemon version matrix
  conformance/
    events/             # Canonical Event JSON fixtures
    wire/               # Logical request/response JSON + canonical msgpack hex
    signatures/         # Event + pubkey + expected verify outcome
    capture.sh          # Regenerates the corpus from a live daemon
```

## How SDKs consume this

Each SDK release pins to a specific commit of this directory. The SDK
build pipeline:

1. Reads `events.schema.json` and `openapi.yaml` and generates
   language-native types.
2. Loads every `wire/<name>.json` fixture, deserializes it through
   the SDK's Request/Response types, re-encodes to MessagePack, and
   asserts the bytes match the corresponding `<name>.msgpack.hex`
   exactly. Drift = build failure.
3. Loads every `signatures/<name>.json`, runs the SDK's signature
   verifier against the embedded event + pubkey, and asserts the
   result matches the `expected` field.
4. Imports `events/<name>.json` fixtures as round-trip test inputs:
   parse → re-serialize → diff.

The Rust workspace runs the same harness in
`crates/ctxd-wire/tests/conformance_corpus.rs` so the daemon is held
to the same standard as the SDKs.

## Regenerating canonical msgpack hex (for maintainers)

The hex files under `conformance/wire/` are the canonical
`rmp-serde` encoding of the corresponding `.json` fixture. They were
authored alongside this PR by hand and verified against the
encoder output. To regenerate them after a deliberate wire-format
change, run the explicit emitter test:

```sh
cargo test -p ctxd-wire --test conformance_emit -- --ignored --nocapture
```

The test prints the hex of every fixture variant. Pipe its output
through `tee` and edit the `wire/*.msgpack.hex` files to match. The
non-`--ignored` conformance test (`conformance_corpus`) will then
re-pass against the new bytes — and any SDK that has not been
updated will fail its own corpus check, which is exactly what we want.

## Regenerating from a live daemon

`conformance/capture.sh` boots a real daemon, mints a capability,
publishes a few sample events, and re-derives the wire / signatures
fixtures from live data. It is the safety net for "if the wire
format ever changes, run this and the corpus updates":

```sh
./docs/api/conformance/capture.sh
```

Requirements: bash, `cargo`, `jq`, `curl`. macOS and Linux only —
this is a developer tool, not a CI dependency.

## Validating the spec

OpenAPI:

```sh
npx @redocly/cli@latest lint docs/api/openapi.yaml
```

JSON Schema (any 2020-12 validator works; example uses `ajv`):

```sh
npx -p ajv-cli ajv compile -s docs/api/events.schema.json --spec=draft2020
```

Both should run clean. CI will gate on these once the SDK pipelines
are wired up.

## Stability guarantees

- **Additive changes** (new optional fields, new endpoints, new enum
  members on response types) are non-breaking and ship in a minor
  release.
- **Field renames, removals, or required-shape changes** are breaking
  and require a major bump (which today means `0.x → 0.(x+1)`).
- The conformance corpus is the regression test. Adding a fixture is
  always allowed; modifying an existing fixture's bytes is a
  deliberate breaking change that must update `COMPATIBILITY.md`.
