//! Loopback integration test for `ProtocolClient`.
//!
//! Spins up a minimal in-process TCP server that speaks the wire codec
//! (just enough to echo a `Pong` and an `Ok` response) and drives it
//! with `ProtocolClient`. This proves:
//!
//! 1. The codec roundtrips a real `Request`/`Response` over a real socket.
//! 2. `ProtocolClient::ping`'s expected-variant check works.
//! 3. The client surfaces errors via `WireError`, not panics.
//!
//! The full daemon-backed protocol tests (PUB/SUB/GRANT exercising
//! `Store` + `CapEngine`) stay in `ctxd-cli` — they need the server
//! stack which is intentionally not visible from here.

use std::sync::Arc;

use ctxd_wire::{
    frame::{read_frame, write_frame},
    ProtocolClient, Request, Response,
};
use tokio::net::TcpListener;
use tokio::sync::Notify;

#[tokio::test]
async fn ping_roundtrips_against_loopback_server() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    let ready = Arc::new(Notify::new());
    let ready_signal = ready.clone();

    let server = tokio::spawn(async move {
        ready_signal.notify_one();
        let (mut stream, _) = listener.accept().await.expect("accept");
        let frame = read_frame(&mut stream)
            .await
            .expect("read frame")
            .expect("some frame");
        let req: Request = rmp_serde::from_slice(&frame).expect("decode req");
        assert!(matches!(req, Request::Ping));
        let bytes = rmp_serde::to_vec(&Response::Pong).expect("encode pong");
        write_frame(&mut stream, &bytes).await.expect("write pong");
    });

    ready.notified().await;
    let mut client = ProtocolClient::connect(&addr.to_string())
        .await
        .expect("connect");
    client.ping().await.expect("ping");
    server.await.expect("server task");
}

#[tokio::test]
async fn unexpected_response_to_ping_surfaces_as_wire_error() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");

    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept");
        let _ = read_frame(&mut stream).await.expect("read frame");
        // Reply with `Ok` to a `Ping` — client must surface this as
        // `WireError::UnexpectedResponse`, not panic.
        let bogus = Response::Ok {
            data: serde_json::json!({"not": "a pong"}),
        };
        let bytes = rmp_serde::to_vec(&bogus).expect("encode");
        write_frame(&mut stream, &bytes).await.expect("write");
    });

    let mut client = ProtocolClient::connect(&addr.to_string())
        .await
        .expect("connect");
    let err = client.ping().await.expect_err("must fail");
    let msg = err.to_string();
    assert!(
        msg.contains("unexpected response"),
        "expected unexpected-response error, got: {msg}"
    );
    server.await.expect("server task");
}
