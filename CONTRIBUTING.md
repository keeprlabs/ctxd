# Contributing to ctxd

Thank you for your interest in contributing to ctxd.

## Getting Started

```bash
git clone https://github.com/ctxd/ctxd.git
cd ctxd
cargo build
cargo test
```

## Development

### Prerequisites

- Rust stable (edition 2021)
- SQLite development libraries (usually pre-installed on macOS/Linux)

### Code Quality

Before submitting a PR, ensure:

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```

### Project Structure

```
crates/
├── ctxd-core/    # Event types, subject paths, hash chains (no deps on other crates)
├── ctxd-store/   # SQLite storage, materialized views (depends on core)
├── ctxd-cap/     # Capability engine via biscuit-auth (depends on core)
├── ctxd-mcp/     # MCP server via rmcp (depends on core, store, cap)
├── ctxd-http/    # Admin REST API via axum (depends on core, store, cap)
└── ctxd-cli/     # Binary that wires everything together
```

### Conventions

- Library crates use `thiserror` for errors. The binary crate uses `anyhow`.
- No `unwrap()` or `expect()` in library code. Fine in tests.
- All public items have rustdoc comments.
- Tests use `tempfile` for SQLite test databases (in-memory or temp files).

## License

By contributing, you agree that your contributions will be licensed under the Apache License 2.0.
