#!/usr/bin/env bash
set -euo pipefail

# ctxd quickstart — write events, read them back, list subjects

echo "=== ctxd quickstart ==="
echo ""

DB=$(mktemp /tmp/ctxd-quickstart-XXXXXX.db)
CTXD="cargo run -q --bin ctxd -- --db $DB"

echo "1. Writing three events..."

$CTXD write \
  --subject /work/acme/customers/cust-42 \
  --type ctx.note \
  --data '{"content":"Interested in enterprise plan","author":"user-1"}'

$CTXD write \
  --subject /work/acme/customers/cust-42 \
  --type ctx.note \
  --data '{"content":"Scheduled demo for next week","author":"user-2"}'

$CTXD write \
  --subject /work/acme/standup/2025-01-15 \
  --type ctx.standup \
  --data '{"participants":["user-1","user-2"],"notes":"Discussed Q1 roadmap"}'

echo ""
echo "2. Reading events for /work/acme/customers/cust-42..."
$CTXD read --subject /work/acme/customers/cust-42

echo ""
echo "3. Reading all events under /work recursively..."
$CTXD read --subject /work --recursive

echo ""
echo "4. Listing all subjects..."
$CTXD subjects

echo ""
echo "5. Listing subjects under /work/acme recursively..."
$CTXD subjects --prefix /work/acme --recursive

echo ""
echo "6. Running EventQL query (basic LIKE filter)..."
$CTXD query 'FROM e IN events WHERE e.subject LIKE "/work/acme/customers/%" PROJECT INTO e'

echo ""
echo "7. Minting a capability token..."
TOKEN=$($CTXD grant --subject "/**" --operations "read,write,subjects,search")
echo "Token: ${TOKEN:0:40}..."

echo ""
echo "8. Verifying the token..."
$CTXD verify --token "$TOKEN" --subject /work/acme/customers/cust-42 --operation read

echo ""
echo "=== quickstart complete ==="
rm -f "$DB"
