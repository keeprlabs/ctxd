//! TCP client for connecting to a running ctxd daemon via the wire protocol.
//!
//! [`ProtocolClient`] is the consumer-facing entry point: it holds an
//! open `TcpStream`, frames a [`Request`], reads back a [`Response`], and
//! is the foundation the upcoming `ctxd-client` Rust SDK builds on top
//! of. It deliberately does not own a runtime, a retry policy, or a
//! connection pool — those are the SDK's job.

use tokio::net::TcpStream;

use crate::errors::{Result, WireError};
use crate::frame::{read_frame, write_frame};
use crate::messages::{Request, Response};

/// TCP client for connecting to a running ctxd daemon via the wire protocol.
pub struct ProtocolClient {
    stream: TcpStream,
}

impl ProtocolClient {
    /// Connect to a ctxd daemon at the given address.
    ///
    /// The address is anything `tokio::net::TcpStream::connect` accepts
    /// (a `host:port` string, a parsed `SocketAddr`, etc.). Returns an
    /// IO error if the daemon isn't reachable.
    pub async fn connect(addr: &str) -> Result<Self> {
        let stream = TcpStream::connect(addr).await?;
        Ok(Self { stream })
    }

    /// Send a request and receive a single response.
    ///
    /// For `Sub`, prefer [`Self::subscribe`] which returns a streaming
    /// reader instead of consuming the connection on a single response.
    pub async fn request(&mut self, req: &Request) -> Result<Response> {
        let data = rmp_serde::to_vec(req)?;
        write_frame(&mut self.stream, &data).await?;

        let frame = read_frame(&mut self.stream)
            .await?
            .ok_or(WireError::ConnectionClosed)?;
        let response: Response = rmp_serde::from_slice(&frame)?;
        Ok(response)
    }

    /// Send a PING and expect a PONG. Useful as a health check.
    pub async fn ping(&mut self) -> Result<()> {
        let response = self.request(&Request::Ping).await?;
        match response {
            Response::Pong => Ok(()),
            other => Err(WireError::UnexpectedResponse(format!(
                "expected Pong, got {other:?}"
            ))),
        }
    }

    /// Publish an event.
    pub async fn publish(
        &mut self,
        subject: &str,
        event_type: &str,
        data: serde_json::Value,
    ) -> Result<Response> {
        self.request(&Request::Pub {
            subject: subject.to_string(),
            event_type: event_type.to_string(),
            data,
        })
        .await
    }

    /// Query a materialized view.
    pub async fn query(&mut self, subject_pattern: &str, view: &str) -> Result<Response> {
        self.request(&Request::Query {
            subject_pattern: subject_pattern.to_string(),
            view: view.to_string(),
        })
        .await
    }

    /// Mint a capability token.
    pub async fn grant(
        &mut self,
        subject: &str,
        operations: &[&str],
        expiry: Option<&str>,
    ) -> Result<Response> {
        self.request(&Request::Grant {
            subject: subject.to_string(),
            operations: operations.iter().map(|s| s.to_string()).collect(),
            expiry: expiry.map(|s| s.to_string()),
        })
        .await
    }

    /// Subscribe to events matching a pattern. Returns the stream for reading events.
    ///
    /// Consumes `self` because the underlying connection is now in
    /// streaming-receive mode — sending another request on the same
    /// socket is a protocol error.
    #[allow(dead_code)]
    pub async fn subscribe(mut self, subject_pattern: &str) -> Result<SubscriptionStream> {
        let req = Request::Sub {
            subject_pattern: subject_pattern.to_string(),
        };
        let data = rmp_serde::to_vec(&req)?;
        write_frame(&mut self.stream, &data).await?;
        Ok(SubscriptionStream {
            stream: self.stream,
        })
    }
}

/// A stream of events from a subscription.
#[allow(dead_code)]
pub struct SubscriptionStream {
    stream: TcpStream,
}

impl SubscriptionStream {
    /// Receive the next event from the subscription.
    ///
    /// Returns `Ok(None)` either when the server sends an explicit
    /// [`Response::EndOfStream`] marker, or when the connection is
    /// closed at a frame boundary.
    #[allow(dead_code)]
    pub async fn next_event(&mut self) -> Result<Option<Response>> {
        let frame = match read_frame(&mut self.stream).await? {
            Some(f) => f,
            None => return Ok(None),
        };
        let response: Response = rmp_serde::from_slice(&frame)?;
        match &response {
            Response::EndOfStream => Ok(None),
            _ => Ok(Some(response)),
        }
    }
}
