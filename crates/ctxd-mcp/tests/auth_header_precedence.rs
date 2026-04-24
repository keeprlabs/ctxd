//! When both an `Authorization: Bearer` header and a per-call
//! `token` argument are present, the header must win.
//!
//! We assert this by minting two tokens with **different scopes**:
//!
//! * `header_token` — grants `Read` on `/scoped/header/**` only.
//! * `arg_token` — grants `Read` on `/scoped/arg/**` only.
//!
//! Then we issue a `ctx_read` call against `/scoped/header/foo`
//! presenting both tokens. If the server uses the arg token, the
//! cap engine will reject (no read scope on `/scoped/header/...`).
//! If it uses the header (correctly), the call succeeds.
//!
//! We also do the inverse — read against `/scoped/arg/foo` with both
//! tokens — and expect a denial: that proves the precedence isn't
//! "always pick whichever works."

mod common;

use ctxd_cap::Operation;
use ctxd_mcp::auth::AuthPolicy;

#[tokio::test]
async fn streamable_http_header_token_beats_arg_token() {
    let (server, cap) = common::make_server().await;
    let store = server.store().clone();
    // Seed two events, one in each subject namespace.
    use ctxd_core::event::Event;
    use ctxd_core::subject::Subject;
    store
        .append(Event::new(
            "test".into(),
            Subject::new("/scoped/header/foo").unwrap(),
            "ctx.note".into(),
            serde_json::json!({"who":"header-event"}),
        ))
        .await
        .unwrap();
    store
        .append(Event::new(
            "test".into(),
            Subject::new("/scoped/arg/foo").unwrap(),
            "ctx.note".into(),
            serde_json::json!({"who":"arg-event"}),
        ))
        .await
        .unwrap();

    let header_token = common::mint(&cap, "/scoped/header/**", &[Operation::Read]);
    let arg_token = common::mint(&cap, "/scoped/arg/**", &[Operation::Read]);

    let (addr, cancel) = common::spawn_streamable_http(server, AuthPolicy::Optional).await;

    // Read /scoped/header/foo with header_token (header) + arg_token (arg).
    // Header wins → cap covers the subject → success.
    let body = common::tools_call_body(
        1,
        "ctx_read",
        serde_json::json!({
            "subject": "/scoped/header/foo",
            "recursive": false,
            "token": arg_token,
        }),
    );
    let auth = format!("Bearer {header_token}");
    let resp = common::http_post(addr, &body, Some(&auth)).await;
    let value = common::parse_http_response(resp).await;
    let text = response_text(&value);
    assert!(
        text.contains("header-event"),
        "header should have authorized — got: {text}"
    );
    assert!(
        !text.contains("error"),
        "header-scoped read should have succeeded: {text}"
    );

    // Read /scoped/arg/foo with header_token (header) + arg_token (arg).
    // Header wins → cap does NOT cover this subject → denial.
    let body = common::tools_call_body(
        2,
        "ctx_read",
        serde_json::json!({
            "subject": "/scoped/arg/foo",
            "recursive": false,
            "token": arg_token,
        }),
    );
    let auth = format!("Bearer {header_token}");
    let resp = common::http_post(addr, &body, Some(&auth)).await;
    let value = common::parse_http_response(resp).await;
    let text = response_text(&value);
    assert!(
        text.contains("error"),
        "header-scoped cap should NOT cover /scoped/arg — but read succeeded: {text}"
    );

    cancel.cancel();
}

#[tokio::test]
async fn sse_header_token_beats_arg_token() {
    let (server, cap) = common::make_server().await;
    let store = server.store().clone();
    use ctxd_core::event::Event;
    use ctxd_core::subject::Subject;
    store
        .append(Event::new(
            "test".into(),
            Subject::new("/scoped/header/foo").unwrap(),
            "ctx.note".into(),
            serde_json::json!({"who":"header-event"}),
        ))
        .await
        .unwrap();
    store
        .append(Event::new(
            "test".into(),
            Subject::new("/scoped/arg/foo").unwrap(),
            "ctx.note".into(),
            serde_json::json!({"who":"arg-event"}),
        ))
        .await
        .unwrap();

    let header_token = common::mint(&cap, "/scoped/header/**", &[Operation::Read]);
    let arg_token = common::mint(&cap, "/scoped/arg/**", &[Operation::Read]);

    let (addr, cancel) = common::spawn_sse(server, AuthPolicy::Optional).await;
    let base = format!("http://{addr}");

    // Open the SSE channel and grab the messages URL.
    let client = reqwest::Client::new();
    let sse = client
        .get(format!("{base}/sse"))
        .header("Accept", "text/event-stream")
        .send()
        .await
        .expect("connect /sse");
    let mut frames = sse_frames::SseFrames::from_response(sse);
    let (_, endpoint_path) = frames
        .next_event(std::time::Duration::from_secs(2))
        .await
        .expect("endpoint event");
    let post_url = format!("{base}{}", endpoint_path.trim());

    // Init handshake.
    client
        .post(&post_url)
        .header("Content-Type", "application/json")
        .body(common::INIT_BODY)
        .send()
        .await
        .unwrap();
    let _init = frames
        .wait_for_id(0, std::time::Duration::from_secs(3))
        .await
        .unwrap();
    client
        .post(&post_url)
        .header("Content-Type", "application/json")
        .body(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
        .send()
        .await
        .unwrap();

    // Read with header that beats the (denied-scope) arg token.
    let body = common::tools_call_body(
        1,
        "ctx_read",
        serde_json::json!({
            "subject": "/scoped/header/foo",
            "recursive": false,
            "token": arg_token,
        }),
    );
    let auth = format!("Bearer {header_token}");
    client
        .post(&post_url)
        .header("Authorization", auth)
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .await
        .unwrap();
    let msg = frames
        .wait_for_id(1, std::time::Duration::from_secs(3))
        .await
        .expect("response");
    let text = response_text(&msg);
    assert!(text.contains("header-event"), "{text}");

    cancel.cancel();
}

fn response_text(value: &serde_json::Value) -> String {
    value
        .get("result")
        .and_then(|r| r.get("content"))
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|i| i.get("text"))
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| value.to_string())
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
