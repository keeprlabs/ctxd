# Capabilities

ctxd uses capability-based authorization via [Biscuit tokens](https://www.biscuitsec.org/).

## Concepts

- **Capabilities, not ACLs.** Access is granted by possessing a signed token, not by being on a list.
- **Attenuable.** A token holder can create a restricted version of their token (narrower scope, fewer operations) and pass it to someone else. They cannot widen it.
- **Bearer tokens.** Whoever holds the token can use it. Protect them like passwords.

## Operations

| Operation | Description |
|-----------|-------------|
| `read` | Read events from subjects |
| `write` | Append events to subjects |
| `subjects` | List subjects |
| `search` | Full-text search events |
| `admin` | Admin operations (mint new tokens) |

## Minting

```bash
# Mint a token with full access
ctxd grant --subject "/**" --operations "read,write,subjects,search"

# Mint a read-only token scoped to /work/**
ctxd grant --subject "/work/**" --operations "read,subjects"
```

The token is output as a base64-encoded string.

## Verification

```bash
ctxd verify --token "<base64>" --subject "/test/hello" --operation read
```

## Attenuation

Tokens can be narrowed via the API. A token for `/**` with `read,write` can be attenuated to `/work/**` with `read` only. The attenuated token is cryptographically bound to the original.

## v0.1 Limitations

- No revocation. A minted token is valid until it expires.
- Token expiry is optional (default: no expiry).
- If no token is provided in an MCP tool call, the operation is allowed (open by default). This is intentional for v0.1 local development.

## Caveat Types

| Caveat | Description |
|--------|-------------|
| Subject glob | Restricts access to subjects matching a glob pattern |
| Operation set | Restricts to a set of operations |
| Expiry | Token becomes invalid after a timestamp |
