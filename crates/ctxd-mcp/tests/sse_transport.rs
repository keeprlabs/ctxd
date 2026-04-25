//! End-to-end test for the legacy MCP SSE transport.
//!
//! Workflow exercised:
//!
//! 1. Open `GET /sse` and read the first `endpoint` event — that
//!    yields the per-session `POST /messages?sessionId=…` URL.
//! 2. POST a `tools/list` JSON-RPC request to that URL.
//! 3. Read the next `message` SSE event and assert it carries our
//!    JSON-RPC response with the expected tool inventory.
//!
//! Then we exercise a tool call (`ctx_write`) the same way, and
//! confirm the response flows back through the same SSE stream.

mod common;

use ctxd_mcp::auth::AuthPolicy;
use futures::StreamExt;
use serde_json::Value;
use std::time::Duration;

#[tokio::test]
async fn sse_endpoint_event_then_tools_list_then_tool_call() {
    let (server, _cap) = common::make_server().await;
    let (addr, cancel) = common::spawn_sse(server, AuthPolicy::Optional).await;
    let base = format!("http://{addr}");

    // Open the SSE stream.
    let client = reqwest::Client::new();
    let sse_resp = client
        .get(format!("{base}/sse"))
        .header("Accept", "text/event-stream")
        .send()
        .await
        .expect("connect /sse");
    assert_eq!(sse_resp.status(), 200);

    let mut events = SseFrames::from_response(sse_resp);

    // First event: endpoint.
    let (event_kind, data) = events
        .next_event(Duration::from_secs(2))
        .await
        .expect("first event");
    assert_eq!(event_kind.as_deref(), Some("endpoint"));
    let post_path = data.trim().to_string();
    assert!(post_path.starts_with("/messages?sessionId="), "{post_path}");

    let post_url = format!("{base}{post_path}");

    // Initialise — this is required by rmcp (we used the full
    // serve flow, not serve_directly, on the SSE transport).
    let resp = client
        .post(&post_url)
        .header("Content-Type", "application/json")
        .body(common::INIT_BODY)
        .send()
        .await
        .expect("post init");
    assert_eq!(resp.status(), 202);

    // Drain SSE responses until we see the init reply (id 0).
    let init_msg = events
        .wait_for_id(0, Duration::from_secs(3))
        .await
        .expect("init response");
    assert!(init_msg.get("result").is_some(), "{init_msg}");

    // Notify the server we're initialised — required handshake step.
    let initialized_notification = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
    let resp = client
        .post(&post_url)
        .header("Content-Type", "application/json")
        .body(initialized_notification)
        .send()
        .await
        .expect("post initialized");
    assert_eq!(resp.status(), 202);

    // tools/list.
    let resp = client
        .post(&post_url)
        .header("Content-Type", "application/json")
        .body(common::TOOLS_LIST_BODY)
        .send()
        .await
        .expect("post tools/list");
    assert_eq!(resp.status(), 202);

    let list_msg = events
        .wait_for_id(1, Duration::from_secs(3))
        .await
        .expect("tools/list response");
    let tools = list_msg
        .get("result")
        .and_then(|r| r.get("tools"))
        .and_then(|t| t.as_array())
        .expect("tools array");
    let names: Vec<String> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(String::from))
        .collect();
    assert!(names.iter().any(|n| n == "ctx_write"), "tools={names:?}");
    assert!(names.iter().any(|n| n == "ctx_read"), "tools={names:?}");

    // tools/call → ctx_write.
    let body = common::tools_call_body(
        2,
        "ctx_write",
        serde_json::json!({
            "subject": "/sse/echo",
            "event_type": "ctx.note",
            "data": r#"{"text":"hi-sse"}"#,
        }),
    );
    let resp = client
        .post(&post_url)
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .await
        .expect("post tools/call");
    assert_eq!(resp.status(), 202);

    let call_msg = events
        .wait_for_id(2, Duration::from_secs(3))
        .await
        .expect("tools/call response");
    let text = call_msg
        .get("result")
        .and_then(|r| r.get("content"))
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|i| i.get("text"))
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();
    assert!(text.contains("/sse/echo"), "{text}");

    cancel.cancel();
}

/// Lightweight SSE frame parser over a streaming reqwest response.
struct SseFrames {
    buffer: String,
    body: futures::stream::BoxStream<'static, Result<bytes::Bytes, reqwest::Error>>,
}

impl SseFrames {
    fn from_response(resp: reqwest::Response) -> Self {
        Self {
            buffer: String::new(),
            body: Box::pin(resp.bytes_stream()),
        }
    }

    /// Read the next complete SSE event (`event:` + `data:` block,
    /// terminated by a blank line). Returns `(event_name, data)`.
    async fn next_event(&mut self, timeout: Duration) -> Option<(Option<String>, String)> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            // Look for a complete event (terminated by a blank line)
            // already in the buffer.
            if let Some(end) = self.buffer.find("\n\n") {
                let frame = self.buffer[..end].to_string();
                self.buffer.drain(..end + 2);
                let mut event = None;
                let mut data = String::new();
                for line in frame.lines() {
                    if let Some(rest) = line.strip_prefix("event: ") {
                        event = Some(rest.to_string());
                    } else if let Some(rest) = line.strip_prefix("event:") {
                        event = Some(rest.trim().to_string());
                    } else if let Some(rest) = line.strip_prefix("data: ") {
                        if !data.is_empty() {
                            data.push('\n');
                        }
                        data.push_str(rest);
                    } else if let Some(rest) = line.strip_prefix("data:") {
                        if !data.is_empty() {
                            data.push('\n');
                        }
                        data.push_str(rest.trim_start());
                    }
                }
                return Some((event, data));
            }

            let now = tokio::time::Instant::now();
            if now >= deadline {
                return None;
            }
            let remaining = deadline - now;
            match tokio::time::timeout(remaining, self.body.next()).await {
                Ok(Some(Ok(chunk))) => {
                    self.buffer.push_str(&String::from_utf8_lossy(&chunk));
                }
                Ok(Some(Err(_))) | Ok(None) => return None,
                Err(_) => return None,
            }
        }
    }

    /// Read SSE events until we find a JSON-RPC response message
    /// whose `id` matches `id`. Skips priming/keep-alive events.
    async fn wait_for_id(&mut self, id: u64, timeout: Duration) -> Option<Value> {
        let deadline = tokio::time::Instant::now() + timeout;
        while tokio::time::Instant::now() < deadline {
            let remaining = deadline - tokio::time::Instant::now();
            let (_kind, data) = self.next_event(remaining).await?;
            if let Ok(v) = serde_json::from_str::<Value>(&data) {
                if v.get("id").and_then(|i| i.as_u64()) == Some(id) {
                    return Some(v);
                }
            }
        }
        None
    }
}
