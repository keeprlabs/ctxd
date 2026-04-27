#!/usr/bin/env bash
# capture.sh — regenerate the ctxd conformance corpus from a live daemon.
#
# Boots `ctxd serve` against an ephemeral SQLite database on a random
# port, mints a wide-open admin capability, publishes a couple of
# sample events, captures the live HTTP responses, and tears down
# cleanly.
#
# Usage:
#   ./docs/api/conformance/capture.sh
#
# Requirements: bash, cargo, jq, curl. macOS and Linux only.
#
# This script is the "if the wire format ever changes, re-derive
# the canonical bytes from a real daemon" safety net. It is NOT a CI
# dependency — CI runs the conformance test that consumes the corpus,
# not this regeneration step.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
HERE="$ROOT/docs/api/conformance"

# Ephemeral working dir for the daemon. Tear it down on exit no
# matter how we leave the script — including on Ctrl-C.
WORKDIR="$(mktemp -d -t ctxd-capture.XXXXXX)"
DAEMON_PID=""
cleanup() {
  if [[ -n "$DAEMON_PID" ]] && kill -0 "$DAEMON_PID" 2>/dev/null; then
    kill "$DAEMON_PID" 2>/dev/null || true
    wait "$DAEMON_PID" 2>/dev/null || true
  fi
  rm -rf "$WORKDIR"
}
trap cleanup EXIT INT TERM

require() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "error: '$1' not found on PATH" >&2
    exit 1
  }
}
require cargo
require jq
require curl

# Pick two free ports. We resolve "free" by binding briefly with
# python; if that's unavailable, we fall back to a deterministic
# range and let the daemon error out if something else holds them.
free_port() {
  python3 - <<'PY' 2>/dev/null || echo "0"
import socket
s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
}

HTTP_PORT="$(free_port)"
WIRE_PORT="$(free_port)"
if [[ -z "$HTTP_PORT" || "$HTTP_PORT" == "0" ]]; then
  HTTP_PORT=17777
fi
if [[ -z "$WIRE_PORT" || "$WIRE_PORT" == "0" ]]; then
  WIRE_PORT=17778
fi

echo ">>> Building ctxd (release-debug) ..."
cargo build --bin ctxd >/dev/null

CTXD="$ROOT/target/debug/ctxd"
if [[ ! -x "$CTXD" ]]; then
  CTXD="$(cargo metadata --no-deps --format-version 1 | jq -r '.target_directory')/debug/ctxd"
fi

DB="$WORKDIR/ctxd.sqlite"

echo ">>> Booting ctxd serve on http=$HTTP_PORT wire=$WIRE_PORT ..."
CTXD_DB="$DB" "$CTXD" serve \
  --bind "127.0.0.1:$HTTP_PORT" \
  --wire-bind "127.0.0.1:$WIRE_PORT" \
  --mcp-stdio false \
  >"$WORKDIR/daemon.log" 2>&1 &
DAEMON_PID=$!

# Wait for /health to come up — try for ~5 seconds.
for _ in $(seq 1 50); do
  if curl -sf "http://127.0.0.1:$HTTP_PORT/health" >/dev/null 2>&1; then
    break
  fi
  sleep 0.1
done

if ! curl -sf "http://127.0.0.1:$HTTP_PORT/health" >/dev/null; then
  echo "error: daemon never came up. log:" >&2
  cat "$WORKDIR/daemon.log" >&2
  exit 1
fi

echo ">>> Minting admin token ..."
GRANT_BODY='{"subject":"/","operations":["read","write","subjects","search","admin"]}'
TOKEN="$(curl -sf -X POST "http://127.0.0.1:$HTTP_PORT/v1/grant" \
  -H 'content-type: application/json' \
  -d "$GRANT_BODY" | jq -r '.token')"
if [[ -z "$TOKEN" || "$TOKEN" == "null" ]]; then
  echo "error: grant did not return a token" >&2
  exit 1
fi

echo ">>> Capturing /health response ..."
curl -sf "http://127.0.0.1:$HTTP_PORT/health" \
  | jq '.' > "$HERE/captured_health.json"

echo ">>> Capturing /v1/stats response ..."
curl -sf "http://127.0.0.1:$HTTP_PORT/v1/stats" \
  | jq '.' > "$HERE/captured_stats.json"

echo ">>> Capturing /v1/peers response (admin) ..."
curl -sf "http://127.0.0.1:$HTTP_PORT/v1/peers" \
  -H "Authorization: Bearer $TOKEN" \
  | jq '.' > "$HERE/captured_peers.json"

echo ">>> Capturing /v1/approvals response ..."
curl -sf "http://127.0.0.1:$HTTP_PORT/v1/approvals" \
  | jq '.' > "$HERE/captured_approvals.json"

cat <<'NOTE'

NOTE: capture.sh writes captured_*.json next to the canonical
fixtures so a maintainer can diff and decide whether to promote a
captured response to a fixture. The canonical wire (msgpack hex)
fixtures and signature fixtures are NOT regenerated automatically
by this script — those depend on deterministic UUIDs and timestamps
and are emitted by the dedicated Rust test:

    cargo test -p ctxd-wire --test conformance_emit -- --ignored

If you intend to update those, run that test, copy the printed
hex into the matching .msgpack.hex file, and re-run the corpus
test to confirm the round-trip.

NOTE
echo ">>> Done. Captured snapshots are under $HERE/captured_*.json."
