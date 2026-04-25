//! Wiremock-driven integration tests for [`OllamaEmbedder`].

#![cfg(feature = "ollama")]

use ctxd_embed::ollama::OllamaEmbedder;
use ctxd_embed::Embedder;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn one_embedding(dims: usize) -> serde_json::Value {
    json!({"embedding": vec![0.05f32; dims]})
}

#[tokio::test]
async fn embed_single_happy_path() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(one_embedding(8)))
        .mount(&server)
        .await;

    let e = OllamaEmbedder::builder()
        .base_url(server.uri())
        .dimensions(8)
        .build()
        .unwrap();
    let v = e.embed("hello").await.unwrap();
    assert_eq!(v.len(), 8);
}

#[tokio::test]
async fn embed_batch_fans_out_per_input() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(one_embedding(8)))
        .expect(5)
        .mount(&server)
        .await;

    let e = OllamaEmbedder::builder()
        .base_url(server.uri())
        .dimensions(8)
        .build()
        .unwrap();
    let inputs = ["a", "b", "c", "d", "e"];
    let out = e.embed_batch(&inputs).await.unwrap();
    assert_eq!(out.len(), 5);
}

#[tokio::test]
async fn embed_retries_on_503() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/embeddings"))
        .respond_with(ResponseTemplate::new(503).set_body_string("model loading"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(one_embedding(8)))
        .mount(&server)
        .await;

    let e = OllamaEmbedder::builder()
        .base_url(server.uri())
        .dimensions(8)
        .build()
        .unwrap();
    let v = e.embed("retry").await.unwrap();
    assert_eq!(v.len(), 8);
}

#[tokio::test]
async fn embed_propagates_404_as_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/embeddings"))
        .respond_with(ResponseTemplate::new(404).set_body_string("model not found"))
        .mount(&server)
        .await;

    let e = OllamaEmbedder::builder()
        .base_url(server.uri())
        .model("nope")
        .dimensions(8)
        .build()
        .unwrap();
    let err = e.embed("x").await.unwrap_err();
    assert!(err.to_string().contains("404"));
}

#[tokio::test]
async fn embed_rejects_empty_input() {
    let server = MockServer::start().await;
    let e = OllamaEmbedder::builder()
        .base_url(server.uri())
        .dimensions(8)
        .build()
        .unwrap();
    let err = e.embed("").await.unwrap_err();
    assert!(err.to_string().contains("empty"));
}
