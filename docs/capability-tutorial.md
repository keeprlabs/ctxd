# Capability Tutorial

This tutorial walks through ctxd's capability-based authorization system. By
the end you will be able to mint tokens, scope them to specific subjects and
operations, attenuate them for sub-agents, and use them with MCP tools.

## 1. What Are Capabilities?

ctxd uses **capability tokens** instead of access control lists. A capability
is a signed bearer token that grants its holder permission to perform certain
operations on certain subjects. Tokens are built on the
[Biscuit](https://www.biscuitsec.org/) format, which means they are:

- **Signed** — forging a token is cryptographically infeasible.
- **Attenuable** — a token holder can create a *narrower* version of their
  token and hand it to someone else. They can never widen it.
- **Bearer** — whoever holds the token can use it. Treat tokens like passwords.

The daemon keeps a root signing key in the SQLite database. All tokens are
verified against this key.

## 2. Minting Your First Token

Start the daemon (or use the CLI directly — the database is shared):

```bash
ctxd serve &
```

Mint a token with broad access:

```bash
ctxd grant --subject "/**" --operations "read,write,subjects,search"
```

This prints a base64-encoded token string. Save it:

```bash
export TOKEN=$(ctxd grant --subject "/**" --operations "read,write,subjects,search")
```

The token grants `read`, `write`, `subjects`, and `search` on every subject
under `/`.

## 3. Scoping a Token

You almost never want a token with `/**` scope. Mint a token that is limited
to a specific subject subtree and a subset of operations:

```bash
# Read-only access to everything under /work/acme
ctxd grant --subject "/work/acme/**" --operations "read,subjects"
```

```bash
# Write access to a single subject
ctxd grant --subject "/work/acme/notes" --operations "write"
```

The `--subject` argument accepts glob patterns:

| Pattern | Matches |
|---------|---------|
| `/**` | Everything |
| `/work/**` | `/work`, `/work/a`, `/work/a/b/c` |
| `/work/*` | `/work/a` but not `/work/a/b` |
| `/work/acme/notes` | Only that exact subject |

The `--operations` argument is a comma-separated list. Valid operations:

| Operation | Description |
|-----------|-------------|
| `read` | Read events from matching subjects |
| `write` | Append events to matching subjects |
| `subjects` | List subjects matching the pattern |
| `search` | Full-text search within matching subjects |
| `admin` | Administrative operations (mint new tokens) |

## 4. Attenuating a Token

Suppose you have a broad token and you want to give a sub-agent access to only
a subset of what you can do. Attenuation creates a new token that is
cryptographically bound to the original but with narrower scope.

```bash
# You hold a token for /work/** with read,write
# Create a narrower token for the sub-agent: /work/acme/** read-only
# (Attenuation is done via the HTTP API in v0.1)
curl -X POST http://127.0.0.1:7777/cap/attenuate \
  -H "Content-Type: application/json" \
  -d '{
    "token": "'$TOKEN'",
    "subject": "/work/acme/**",
    "operations": ["read"]
  }'
```

The attenuated token can be passed to the sub-agent. The sub-agent cannot
escalate it back to `write` or widen the subject scope — the Biscuit
cryptographic chain prevents this.

## 5. Verifying Tokens

To check whether a token is valid for a specific operation on a specific
subject:

```bash
ctxd verify --token "$TOKEN" --subject "/work/acme/notes" --operation read
```

Output on success:

```
VERIFIED: token is valid for read on /work/acme/notes
```

Output on failure:

```
DENIED: subject /work/acme/notes does not match token scope
```

Verification checks three things:

1. The token signature is valid (signed by this daemon's root key).
2. The subject matches the token's glob pattern.
3. The requested operation is in the token's allowed set.

## 6. Token Expiry

By default, tokens do not expire. In v0.1 expiry is set via the API when
minting:

```bash
# Mint a token that expires in 1 hour (3600 seconds)
curl -X POST http://127.0.0.1:7777/cap/grant \
  -H "Content-Type: application/json" \
  -d '{
    "subject": "/work/**",
    "operations": ["read", "write"],
    "ttl_seconds": 3600
  }'
```

After expiry, verification will return `DENIED`. There is no revocation
mechanism in v0.1 — a minted token is valid until it expires.

## 7. Using Tokens with MCP Tools

When connecting to ctxd via MCP (e.g., from Claude Desktop), pass the token in
the `token` parameter of any tool call:

```json
{
  "tool": "ctx_write",
  "arguments": {
    "subject": "/work/acme/notes",
    "event_type": "ctx.note",
    "data": "{\"content\": \"meeting notes from today\"}",
    "token": "<base64-token>"
  }
}
```

```json
{
  "tool": "ctx_read",
  "arguments": {
    "subject": "/work/acme/notes",
    "token": "<base64-token>"
  }
}
```

If the token is missing, the operation is allowed by default in v0.1. This
makes local development easy — you do not need to mint tokens to get started.
A future `--require-auth` flag will change this behavior for production use.

## Quick Reference

```bash
# Mint a broad token
ctxd grant --subject "/**" --operations "read,write,subjects,search"

# Mint a scoped, read-only token
ctxd grant --subject "/work/acme/**" --operations "read,subjects"

# Verify a token
ctxd verify --token "$TOKEN" --subject "/work/acme/notes" --operation read

# Use in a write command
ctxd write --subject "/test/hello" --type demo --data '{"msg":"hi"}'
```
