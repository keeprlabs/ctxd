# ctxd-dashboard

Embedded web UI for the ctxd substrate. Frontend assets baked via [`rust-embed`](https://crates.io/crates/rust-embed); served from the same axum router as the JSON API.

See [`docs/dashboard.md`](../../docs/dashboard.md) for the user-facing guide (what each view shows, security model, troubleshooting).

## What's here

- `src/lib.rs` — `pub fn router()` returning an axum `Router` for `GET /` and `GET /static/{*path}`.
- `src/static_assets.rs` — `rust-embed` wrapper. Six-arm `mime_for_path` covers `.html .css .js .svg .ico .woff2`. ETag from sha256 prefix. Path-traversal rejection.
- `src/middleware.rs` — `localhost_or_cap_token`. Loopback peer → allow; otherwise require an admin capability token. Fails closed (500) if the bind site forgot `into_make_service_with_connect_info::<SocketAddr>()`.
- `assets/` — vanilla HTML / CSS / JS. No build step. Read from disk in debug builds for hot-reload; baked into the binary in release builds.
