//! Stdio-to-HTTP MCP proxy.
//!
//! When `ctxd serve --mcp-stdio --cap-file <path>` is invoked as a
//! subprocess by an MCP client (Claude Desktop, Claude Code, Codex)
//! AND a long-running ctxd daemon already owns the same SQLite DB,
//! this subprocess MUST NOT open a second handle on that DB. Instead
//! it becomes a thin proxy: read JSON-RPC frames from stdin, POST
//! each one to the running daemon's `/mcp` HTTP endpoint with the
//! cap-file token as bearer auth, write the response back to stdout.
//!
//! ## Why this exists
//!
//! Without it, every Claude Desktop / Code conversation spawns a
//! parallel `ctxd serve --mcp-stdio` subprocess that opens
//! `ctxd.db` directly. Two processes writing the same SQLite file
//! corrupts it. The phase 1A pidfile lock prevents the worst case
//! (the second process refuses to start) but that breaks the MCP
//! client's tool calls. The proxy is the actually-correct fix:
//! shared memory across all tools, single writer to the DB.
//!
//! ## Wire format
//!
//! Stdin: newline-delimited JSON-RPC 2.0 messages. Each line is one
//! request. Empty lines are tolerated.
//!
//! HTTP: `POST <daemon>/mcp` with:
//! * `Content-Type: application/json`
//! * `Accept: application/json, text/event-stream`
//! * `Authorization: Bearer <base64-cap>`
//! * Body: the JSON-RPC line read from stdin.
//!
//! Stdout: the daemon's response JSON, one message per line. The
//! daemon is configured with `with_json_response(true)` (see
//! `crates/ctxd-mcp/src/transport/streamable_http.rs`), which means
//! stateless tool calls return a single JSON body rather than an SSE
//! stream — perfect for line-oriented stdio relay.

use anyhow::{Context, Result};
use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Run the proxy until stdin closes.
///
/// `daemon_url` is `http://<admin_bind>` from the pidfile (we pull
/// the MCP HTTP bind by convention — the launchd plist installed by
/// onboard always uses port 7780, so we replace the admin port with
/// 7780 to compute the MCP URL).
///
/// `cap_b64` is the bearer token read from the cap-file.
pub async fn run(daemon_admin: &str, cap_b64: &str) -> Result<()> {
    let mcp_url = derive_mcp_url(daemon_admin);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .context("build reqwest client for MCP proxy")?;

    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin);
    let mut stdout = tokio::io::stdout();

    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await.context("read stdin")?;
        if n == 0 {
            // EOF — client closed stdin.
            return Ok(());
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Forward the request body untouched. Don't try to parse it
        // beyond confirming it's not empty — the daemon validates.
        let resp = client
            .post(&mcp_url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(
                reqwest::header::ACCEPT,
                "application/json, text/event-stream",
            )
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {cap_b64}"))
            .body(trimmed.to_string())
            .send()
            .await;
        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(error = %e, "MCP proxy request failed; emitting JSON-RPC error");
                emit_error(&mut stdout, trimmed, -32000, &format!("ctxd proxy: {e}")).await?;
                continue;
            }
        };
        let status = resp.status();
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/json")
            .to_string();
        let body = match resp.text().await {
            Ok(b) => b,
            Err(e) => {
                emit_error(
                    &mut stdout,
                    trimmed,
                    -32000,
                    &format!("ctxd proxy: read body: {e}"),
                )
                .await?;
                continue;
            }
        };
        if !status.is_success() {
            emit_error(
                &mut stdout,
                trimmed,
                -32000,
                &format!("ctxd proxy: daemon returned {status}: {body}"),
            )
            .await?;
            continue;
        }
        if content_type.starts_with("text/event-stream") {
            // Daemon negotiated SSE — extract `data:` lines and emit
            // each as an MCP frame on stdout.
            for ev_line in body.lines() {
                if let Some(payload) = ev_line.strip_prefix("data:") {
                    let payload = payload.trim_start();
                    if payload.is_empty() {
                        continue;
                    }
                    stdout.write_all(payload.as_bytes()).await?;
                    stdout.write_all(b"\n").await?;
                }
            }
            stdout.flush().await?;
        } else if !body.trim().is_empty() {
            // Plain JSON response — write as one line. An empty body
            // means the daemon answered 202 Accepted for a JSON-RPC
            // notification (no `id`, no response expected); emitting
            // even a bare newline would make line-oriented MCP
            // clients try to JSON.parse("") and surface "Unexpected
            // end of JSON input" — see Claude Desktop log around the
            // notifications/initialized handshake. Some servers
            // pretty-print; collapse to single line so the client's
            // line reader sees one frame.
            let single_line = body.replace('\n', "");
            stdout.write_all(single_line.as_bytes()).await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;
        }
    }
}

/// Translate the daemon's admin URL (`127.0.0.1:7777`) into the MCP
/// HTTP transport URL by convention. Onboard's launchd plist always
/// binds `--mcp-http 127.0.0.1:7780`, so we swap the port here.
///
/// If the admin URL has any other port, fall back to using the
/// admin port + 3 (so :8000 → :8003, etc) — same convention but
/// scaled. This is brittle; phase 3B+ should publish the MCP URL
/// in the pidfile directly.
pub fn derive_mcp_url(admin_bind: &str) -> String {
    let mcp_bind = match admin_bind.rsplit_once(':') {
        Some((host, port)) => match port.parse::<u16>() {
            Ok(7777) => format!("{host}:7780"),
            Ok(p) => format!("{host}:{}", p + 3),
            Err(_) => admin_bind.to_string(),
        },
        None => admin_bind.to_string(),
    };
    format!("http://{mcp_bind}/mcp")
}

/// Emit a JSON-RPC error response on stdout. Best-effort id extraction
/// from the original request so the MCP client correlates correctly.
async fn emit_error(
    stdout: &mut tokio::io::Stdout,
    original_req: &str,
    code: i32,
    message: &str,
) -> Result<()> {
    let id = serde_json::from_str::<serde_json::Value>(original_req)
        .ok()
        .and_then(|v| v.get("id").cloned())
        .unwrap_or(serde_json::Value::Null);
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    });
    let line = serde_json::to_string(&body).unwrap_or_else(|_| "{}".to_string());
    stdout.write_all(line.as_bytes()).await?;
    stdout.write_all(b"\n").await?;
    stdout.flush().await?;
    Ok(())
}

/// Read a cap-file from disk. Returns the trimmed base64 contents.
pub fn read_cap_file(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("read cap-file {path:?}"))?;
    let s = String::from_utf8(bytes).context("cap-file is not valid UTF-8")?;
    Ok(s.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_mcp_url_swaps_7777_for_7780() {
        assert_eq!(
            derive_mcp_url("127.0.0.1:7777"),
            "http://127.0.0.1:7780/mcp"
        );
    }

    #[test]
    fn derive_mcp_url_offsets_other_ports_by_3() {
        assert_eq!(
            derive_mcp_url("127.0.0.1:8000"),
            "http://127.0.0.1:8003/mcp"
        );
    }

    #[test]
    fn derive_mcp_url_passes_through_no_port() {
        // Pathological input — no colon. Return as-is wrapped in /mcp.
        assert_eq!(derive_mcp_url("nohost"), "http://nohost/mcp");
    }
}
