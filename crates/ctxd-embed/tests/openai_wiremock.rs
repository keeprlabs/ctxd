//! Wiremock-driven integration tests for [`OpenAiEmbedder`].
//!
//! All tests in this file are gated on `feature = "openai"` so a
//! default `cargo test --workspace` run skips them. They cover the
//! happy path, a 429-with-Retry-After backoff, batching, and a
//! non-retryable 4xx status.

#![cfg(feature = "openai")]

use ctxd_embed::openai::OpenAiEmbedder;
use ctxd_embed::Embedder;
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn embedding_response(n: usize) -> serde_json::Value {
    let data: Vec<_> = (0..n)
        .map(|i| {
            json!({
                "object": "embedding",
                "index": i,
                "embedding": vec![0.1f32; 8],
            })
        })
        .collect();
    json!({
        "object": "list",
        "data": data,
        "model": "text-embedding-3-small",
        "usage": {"prompt_tokens": 1, "total_tokens": 1}
    })
}

#[tokio::test]
async fn embed_single_happy_path() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/embeddings"))
        .and(header("authorization", "Bearer test-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(embedding_response(1)))
        .mount(&server)
        .await;

    let embedder = OpenAiEmbedder::builder()
        .api_key("test-key")
        .base_url(server.uri())
        .dimensions(8)
        .build()
        .unwrap();

    let v = embedder.embed("hello world").await.unwrap();
    assert_eq!(v.len(), 8);
    assert!((v[0] - 0.1).abs() < 1e-6);
}

#[tokio::test]
async fn embed_batch_chunks_at_256() {
    // 300 inputs => 2 requests: one of 256 + one of 44.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/embeddings"))
        .respond_with(move |req: &wiremock::Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            let n = body["input"].as_array().unwrap().len();
            ResponseTemplate::new(200).set_body_json(embedding_response(n))
        })
        .expect(2)
        .mount(&server)
        .await;

    let embedder = OpenAiEmbedder::builder()
        .api_key("k")
        .base_url(server.uri())
        .dimensions(8)
        .build()
        .unwrap();

    let inputs: Vec<String> = (0..300).map(|i| format!("text-{i}")).collect();
    let refs: Vec<&str> = inputs.iter().map(String::as_str).collect();
    let out = embedder.embed_batch(&refs).await.unwrap();
    assert_eq!(out.len(), 300);
}

#[tokio::test]
async fn embed_retries_on_429_with_retry_after() {
    let server = MockServer::start().await;
    // First call: 429 with a 0.1s retry-after.
    Mock::given(method("POST"))
        .and(path("/embeddings"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "0.1")
                .set_body_string("rate limited"),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    // Subsequent: success.
    Mock::given(method("POST"))
        .and(path("/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(embedding_response(1)))
        .mount(&server)
        .await;

    let embedder = OpenAiEmbedder::builder()
        .api_key("k")
        .base_url(server.uri())
        .dimensions(8)
        .build()
        .unwrap();

    let v = embedder.embed("retry me").await.unwrap();
    assert_eq!(v.len(), 8);
}

#[tokio::test]
async fn embed_returns_error_on_401() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/embeddings"))
        .respond_with(ResponseTemplate::new(401).set_body_string("invalid api key"))
        .mount(&server)
        .await;

    let embedder = OpenAiEmbedder::builder()
        .api_key("bad-key")
        .base_url(server.uri())
        .dimensions(8)
        .build()
        .unwrap();

    let err = embedder.embed("doesn't matter").await.unwrap_err();
    let msg = err.to_string();
    // The error must surface 401, but must NOT echo our key.
    assert!(msg.contains("401"), "expected 401 in {msg}");
    assert!(!msg.contains("bad-key"), "api key leaked in: {msg}");
}

#[tokio::test]
async fn embed_rejects_empty_input() {
    let server = MockServer::start().await;
    let embedder = OpenAiEmbedder::builder()
        .api_key("k")
        .base_url(server.uri())
        .dimensions(8)
        .build()
        .unwrap();
    let err = embedder.embed("").await.unwrap_err();
    assert!(err.to_string().contains("empty"));
}
