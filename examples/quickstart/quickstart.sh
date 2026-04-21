#!/usr/bin/env bash
set -euo pipefail

# ctxd quickstart — write events, read them back, list subjects

echo "=== ctxd quickstart ==="
echo ""

DB=$(mktemp /tmp/ctxd-quickstart-XXXXXX.db)
CTXD="cargo run -q -- --db $DB"

echo "1. Writing three events..."

$CTXD write \
  --subject /work/exlo/customers/dmitry \
  --type ctx.note \
  --data '{"content":"Interested in enterprise plan","author":"alice"}'

$CTXD write \
  --subject /work/exlo/customers/dmitry \
  --type ctx.note \
  --data '{"content":"Scheduled demo for next week","author":"bob"}'

$CTXD write \
  --subject /work/exlo/standup/2025-01-15 \
  --type ctx.standup \
  --data '{"participants":["alice","bob"],"notes":"Discussed Q1 roadmap"}'

echo ""
echo "2. Reading events for /work/exlo/customers/dmitry..."
$CTXD read --subject /work/exlo/customers/dmitry

echo ""
echo "3. Reading all events under /work recursively..."
$CTXD read --subject /work --recursive

echo ""
echo "4. Listing all subjects..."
$CTXD subjects

echo ""
echo "5. Listing subjects under /work/exlo recursively..."
$CTXD subjects --prefix /work/exlo --recursive

echo ""
echo "6. Running EventQL query (basic LIKE filter)..."
$CTXD query 'FROM e IN events WHERE e.subject LIKE "/work/exlo/customers/%" PROJECT INTO e'

echo ""
echo "7. Minting a capability token..."
TOKEN=$($CTXD grant --subject "/**" --operations "read,write,subjects,search")
echo "Token: ${TOKEN:0:40}..."

echo ""
echo "8. Verifying the token..."
$CTXD verify --token "$TOKEN" --subject /work/exlo/customers/dmitry --operation read

echo ""
echo "=== quickstart complete ==="
rm -f "$DB"
