//! Error type for the wire protocol layer.
//!
//! Kept deliberately small: the wire crate only owns codec / IO concerns.
//! Higher layers (the daemon's `ProtocolServer`, federation, the upcoming
//! `ctxd-client` SDK) are free to wrap [`WireError`] in their own richer
//! error types via `thiserror`'s `#[from]` or `Display`-based glue.

use std::io;

use thiserror::Error;

/// Errors raised by the wire-protocol codec and TCP client.
#[derive(Debug, Error)]
pub enum WireError {
    /// Underlying IO failure on the TCP stream.
    #[error("io error: {0}")]
    Io(#[from] io::Error),

    /// Frame length-prefix exceeded [`crate::frame::MAX_FRAME_BYTES`].
    /// The wire format is bounded so a malicious or buggy peer cannot
    /// trigger an unbounded allocation.
    #[error("frame too large: {len} bytes (max {max})")]
    FrameTooLarge {
        /// Length the peer claimed.
        len: usize,
        /// The configured upper bound.
        max: usize,
    },

    /// MessagePack encoding (serializing a `Request` or `Response`) failed.
    #[error("encode error: {0}")]
    Encode(#[from] rmp_serde::encode::Error),

    /// MessagePack decoding (deserializing a frame) failed.
    #[error("decode error: {0}")]
    Decode(#[from] rmp_serde::decode::Error),

    /// The remote closed the TCP connection before sending an expected
    /// response. Surfaced separately from raw IO so callers can choose
    /// to reconnect rather than treat it as a hard failure.
    #[error("server closed connection")]
    ConnectionClosed,

    /// The server returned a response variant that did not match what
    /// the client requested (e.g. PING that came back as `Ok` instead
    /// of `Pong`).
    #[error("unexpected response: {0}")]
    UnexpectedResponse(String),
}

/// Convenience alias used throughout the wire crate.
pub type Result<T> = std::result::Result<T, WireError>;
