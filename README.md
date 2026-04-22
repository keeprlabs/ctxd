# ctxd

A context substrate for AI agents. Single-binary daemon that ingests, stores, addresses, and serves personal/organizational context over MCP.

Think "NATS for context" or "Postgres for AI memory." Not a vector DB, not an agent framework, not a knowledge graph, not an LLM. It's a substrate.

## Quickstart (60 seconds)

```bash
# Build
cargo build --release

# Write an event
./target/release/ctxd write \
  --subject /test/hello \
  --type demo \
  --data '{"msg":"world"}'

# Read it back
./target/release/ctxd read --subject /test --recursive

# List subjects
./target/release/ctxd subjects

# Start the daemon (HTTP on :7777, MCP on stdio)
./target/release/ctxd serve
```

## Connect via MCP

Add to your Claude Desktop config (`claude_desktop_config.json`):

```json
{
  "mcpServers": {
    "ctxd": {
      "command": "/path/to/ctxd",
      "args": ["serve", "--mcp-stdio"]
    }
  }
}
```

Then Claude can use `ctx_write`, `ctx_read`, and `ctx_subjects` tools.

## Architecture

- **Event log is the source of truth.** Append-only, all views are derived.
- **CloudEvents v1.0 spec.** Standard event format with ctxd extensions.
- **Subjects are paths.** `/work/acme/customers/cust-42` with recursive reads and glob wildcards.
- **Predecessor hash chains.** SHA-256 tamper-evidence without consensus.
- **Capabilities, not ACLs.** Biscuit tokens: signed, attenuable, bearer.
- **SQLite.** Single-binary, zero-config. Other backends come later.

See [docs/architecture.md](docs/architecture.md) for the full picture.

## CLI Reference

```
ctxd serve              # Start daemon (HTTP + MCP)
ctxd write              # Append an event
ctxd read               # Read events for a subject
ctxd query              # Run an EventQL query (v0.1: basic LIKE filter)
ctxd subjects           # List subjects
ctxd grant              # Mint a capability token
ctxd verify             # Verify a capability token
```

## Project Structure

```
crates/
├── ctxd-core/    # Event types, subject paths, hash chains
├── ctxd-store/   # SQLite storage, materialized views
├── ctxd-cap/     # Capability engine (biscuit-auth)
├── ctxd-mcp/     # MCP server (rmcp)
├── ctxd-http/    # Admin REST API (axum)
└── ctxd-cli/     # The ctxd binary
```

## Development

```bash
cargo test          # Run tests
cargo clippy        # Lint
cargo fmt --check   # Check formatting
```

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

Apache-2.0
