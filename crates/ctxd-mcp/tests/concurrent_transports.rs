//! Run all three transports against the same `CtxdMcpServer`
//! instance, fire workloads on each in parallel, and assert the
//! transports do not block each other.
//!
//! "Isolation" here means: a tool call in flight on one transport
//! must not stall the same call on another. We exercise this by
//! issuing 16 concurrent `ctx_write` calls split between
//! streamable-HTTP and SSE, plus a concurrent in-process call on
//! the stdio surface (we don't fork a subprocess for stdio — the
//! shared state is the only thing under test).

mod common;

use ctxd_cap::state::{CaveatState, InMemoryCaveatState};
use ctxd_cap::CapEngine;
use ctxd_core::event::Event;
use ctxd_core::subject::Subject;
use ctxd_mcp::auth::AuthPolicy;
use ctxd_mcp::CtxdMcpServer;
use ctxd_store::EventStore;
use std::sync::Arc;
use tokio::time::{Duration, Instant};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_transports_share_one_server_without_blocking() {
    let store = EventStore::open_memory().await.expect("open store");
    let cap = Arc::new(CapEngine::new());
    let caveat_state: Arc<dyn CaveatState> = Arc::new(InMemoryCaveatState::new());
    let server = CtxdMcpServer::new(
        store.clone(),
        cap.clone(),
        caveat_state,
        "ctxd://test".into(),
    );

    let (http_addr, http_cancel) =
        common::spawn_streamable_http(server.clone(), AuthPolicy::Optional).await;
    let (sse_addr, sse_cancel) = common::spawn_sse(server.clone(), AuthPolicy::Optional).await;

    // Pre-seed an event so the stdio task has something to read.
    store
        .append(Event::new(
            "test".into(),
            Subject::new("/concurrent/seed").unwrap(),
            "ctx.note".into(),
            serde_json::json!({"who":"seed"}),
        ))
        .await
        .unwrap();

    let start = Instant::now();
    let workload = 8;

    // HTTP fan-out.
    let http_handles: Vec<_> = (0..workload)
        .map(|i| {
            let body = common::tools_call_body(
                100 + i,
                "ctx_write",
                serde_json::json!({
                    "subject": format!("/concurrent/http/{i}"),
                    "event_type": "ctx.note",
                    "data": "{}",
                }),
            );
            tokio::spawn(async move {
                let resp = common::http_post(http_addr, &body, None).await;
                assert_eq!(resp.status(), 200, "http call #{i} failed");
            })
        })
        .collect();

    // SSE fan-out — each client opens its own session for parity.
    let sse_handles: Vec<_> = (0..workload)
        .map(|i| {
            let base = format!("http://{sse_addr}");
            tokio::spawn(async move {
                let client = reqwest::Client::new();
                let sse = client
                    .get(format!("{base}/sse"))
                    .header("Accept", "text/event-stream")
                    .send()
                    .await
                    .expect("connect /sse");
                let mut frames = SseFrames::from_response(sse);
                let (_, endpoint_path) = frames.next_event(Duration::from_secs(2)).await.unwrap();
                let post_url = format!("{base}{}", endpoint_path.trim());
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
                let body = common::tools_call_body(
                    200 + i,
                    "ctx_write",
                    serde_json::json!({
                        "subject": format!("/concurrent/sse/{i}"),
                        "event_type": "ctx.note",
                        "data": "{}",
                    }),
                );
                client
                    .post(&post_url)
                    .header("Content-Type", "application/json")
                    .body(body)
                    .send()
                    .await
                    .unwrap();
                let resp = frames
                    .wait_for_id(200 + i, Duration::from_secs(3))
                    .await
                    .unwrap_or_else(|| panic!("sse #{i} no response"));
                assert!(resp.get("result").is_some(), "sse #{i}: {resp}");
            })
        })
        .collect();

    // Stdio surrogate: in-process reads against the shared store.
    // The point of this arm is to assert the shared state (cap
    // engine, store) does not get monopolised by HTTP/SSE traffic —
    // not to actually exercise rmcp's stdio framing (impractical
    // from inside a #[tokio::test]).
    let stdio_store = server.store().clone();
    let stdio_handle = tokio::spawn(async move {
        for _ in 0..workload {
            let events = stdio_store
                .read(&Subject::new("/concurrent/seed").unwrap(), false)
                .await
                .expect("stdio read");
            assert_eq!(
                events.len(),
                1,
                "stdio read returned {} events",
                events.len()
            );
        }
    });

    for h in http_handles {
        h.await.unwrap();
    }
    for h in sse_handles {
        h.await.unwrap();
    }
    stdio_handle.await.unwrap();

    let elapsed = start.elapsed();
    // Loose budget — we're not measuring throughput, just guarding
    // against a deadlock-style regression.
    assert!(
        elapsed < Duration::from_secs(10),
        "took too long: {elapsed:?}"
    );

    // Final sanity: store should now contain workload events under
    // each HTTP and SSE prefix.
    let http_events = store
        .read(&Subject::new("/concurrent/http").unwrap(), true)
        .await
        .unwrap();
    let sse_events = store
        .read(&Subject::new("/concurrent/sse").unwrap(), true)
        .await
        .unwrap();
    assert_eq!(http_events.len(), workload as usize);
    assert_eq!(sse_events.len(), workload as usize);

    http_cancel.cancel();
    sse_cancel.cancel();
}

// Local SSE frame helper duplicated from auth_header_precedence.rs —
// each integration test file is its own crate, so common helpers
// either live in `tests/common/mod.rs` (limited API) or are inlined
// per file. The duplication is intentional and small.
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
    async fn next_event(&mut self, timeout: Duration) -> Option<(Option<String>, String)> {
        use futures::StreamExt;
        let deadline = Instant::now() + timeout;
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
            let now = Instant::now();
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
    async fn wait_for_id(&mut self, id: u64, timeout: Duration) -> Option<serde_json::Value> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let remaining = deadline - Instant::now();
            let (_, data) = self.next_event(remaining).await?;
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) {
                if v.get("id").and_then(|i| i.as_u64()) == Some(id) {
                    return Some(v);
                }
            }
        }
        None
    }
}
