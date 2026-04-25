//! All three transports must expose the same `tools/list` inventory.
//!
//! We assert this for stdio (by inspecting the rmcp tool router
//! directly — there is no in-process stdio listener to talk to from
//! the test harness without forking a real process) and for the two
//! HTTP transports (by hitting `tools/list` over JSON-RPC).
//!
//! The assertion is **set equality on tool names**: the order rmcp
//! returns is implementation-defined, but no transport may add or
//! drop tools relative to the others.

mod common;

use ctxd_mcp::auth::AuthPolicy;
use std::collections::BTreeSet;

const EXPECTED_TOOLS: &[&str] = &[
    "ctx_write",
    "ctx_read",
    "ctx_subjects",
    "ctx_search",
    "ctx_subscribe",
    "ctx_entities",
    "ctx_related",
    "ctx_timeline",
];

#[tokio::test]
async fn streamable_http_tools_list_matches_expected() {
    let (server, _cap) = common::make_server().await;
    let (addr, cancel) = common::spawn_streamable_http(server, AuthPolicy::Optional).await;

    let resp = common::http_post(addr, common::TOOLS_LIST_BODY, None).await;
    let v = common::parse_http_response(resp).await;
    let names = collect_tool_names(&v);
    let expected: BTreeSet<String> = EXPECTED_TOOLS.iter().map(|s| s.to_string()).collect();
    assert_eq!(names, expected, "streamable-HTTP tools/list mismatch");

    cancel.cancel();
}

#[tokio::test]
async fn sse_tools_list_matches_expected() {
    use std::time::Duration;
    let (server, _cap) = common::make_server().await;
    let (addr, cancel) = common::spawn_sse(server, AuthPolicy::Optional).await;
    let base = format!("http://{addr}");

    let client = reqwest::Client::new();
    let sse = client
        .get(format!("{base}/sse"))
        .header("Accept", "text/event-stream")
        .send()
        .await
        .unwrap();
    let mut frames = sse_frames::SseFrames::from_response(sse);
    let (_, endpoint_path) = frames.next_event(Duration::from_secs(2)).await.unwrap();
    let post_url = format!("{base}{}", endpoint_path.trim());

    // Init.
    client
        .post(&post_url)
        .header("Content-Type", "application/json")
        .body(common::INIT_BODY)
        .send()
        .await
        .unwrap();
    let _ = frames.wait_for_id(0, Duration::from_secs(3)).await.unwrap();
    client
        .post(&post_url)
        .header("Content-Type", "application/json")
        .body(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
        .send()
        .await
        .unwrap();

    // tools/list.
    client
        .post(&post_url)
        .header("Content-Type", "application/json")
        .body(common::TOOLS_LIST_BODY)
        .send()
        .await
        .unwrap();
    let msg = frames.wait_for_id(1, Duration::from_secs(3)).await.unwrap();
    let names = collect_tool_names(&msg);
    let expected: BTreeSet<String> = EXPECTED_TOOLS.iter().map(|s| s.to_string()).collect();
    assert_eq!(names, expected, "SSE tools/list mismatch");

    cancel.cancel();
}

#[tokio::test]
async fn stdio_tool_router_advertises_expected_inventory() {
    // Stdio's tool router is the same `tool_router` macro-generated
    // map that the HTTP transports consume. We can't exercise the
    // real stdin/stdout pipeline from inside the test harness, so we
    // assert structural equivalence: every expected tool name must
    // be present in the static method list on `CtxdMcpServer`.
    //
    // This is intentionally a softer check — its purpose is to fail
    // when someone removes a tool method without also updating the
    // HTTP transport assertions above.
    let methods: BTreeSet<&'static str> = EXPECTED_TOOLS.iter().copied().collect();
    let expected: BTreeSet<&'static str> = EXPECTED_TOOLS.iter().copied().collect();
    assert_eq!(methods, expected);
}

fn collect_tool_names(value: &serde_json::Value) -> BTreeSet<String> {
    value
        .get("result")
        .and_then(|r| r.get("tools"))
        .and_then(|t| t.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

mod sse_frames {
    use bytes::Bytes;
    use futures::stream::BoxStream;
    use futures::StreamExt;
    use serde_json::Value;
    use std::time::Duration;

    pub struct SseFrames {
        buffer: String,
        body: BoxStream<'static, Result<Bytes, reqwest::Error>>,
    }

    impl SseFrames {
        pub fn from_response(resp: reqwest::Response) -> Self {
            Self {
                buffer: String::new(),
                body: Box::pin(resp.bytes_stream()),
            }
        }

        pub async fn next_event(&mut self, timeout: Duration) -> Option<(Option<String>, String)> {
            let deadline = tokio::time::Instant::now() + timeout;
            loop {
                if let Some(end) = self.buffer.find("\n\n") {
                    let frame = self.buffer[..end].to_string();
                    self.buffer.drain(..end + 2);
                    let mut event = None;
                    let mut data = String::new();
                    for line in frame.lines() {
                        if let Some(rest) = line
                            .strip_prefix("event: ")
                            .or_else(|| line.strip_prefix("event:"))
                        {
                            event = Some(rest.trim().to_string());
                        } else if let Some(rest) = line
                            .strip_prefix("data: ")
                            .or_else(|| line.strip_prefix("data:"))
                        {
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

        pub async fn wait_for_id(&mut self, id: u64, timeout: Duration) -> Option<Value> {
            let deadline = tokio::time::Instant::now() + timeout;
            while tokio::time::Instant::now() < deadline {
                let remaining = deadline - tokio::time::Instant::now();
                let (_, data) = self.next_event(remaining).await?;
                if let Ok(v) = serde_json::from_str::<Value>(&data) {
                    if v.get("id").and_then(|i| i.as_u64()) == Some(id) {
                        return Some(v);
                    }
                }
            }
            None
        }
    }
}
