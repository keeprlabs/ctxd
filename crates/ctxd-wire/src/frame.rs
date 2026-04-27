//! Length-prefixed frame codec for the wire protocol.
//!
//! Each message is encoded as a 4-byte big-endian unsigned length followed
//! by exactly that many bytes of MessagePack. The framing layer is
//! deliberately payload-agnostic — the same helpers carry [`Request`]s,
//! [`Response`]s, and federation messages.
//!
//! [`Request`]: crate::messages::Request
//! [`Response`]: crate::messages::Response

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::errors::{Result, WireError};

/// Maximum frame size accepted by [`read_frame`]. Frames larger than this
/// are rejected with [`WireError::FrameTooLarge`] before any allocation
/// is performed, so a malicious peer cannot OOM the process by sending
/// a 4-byte header claiming gigabytes of payload.
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// Read a length-prefixed MessagePack frame from a stream.
///
/// Returns `Ok(None)` if the peer cleanly closed the connection at a
/// frame boundary (i.e. before sending the next 4-byte length). Returns
/// `Ok(Some(bytes))` for a successfully read frame, or [`WireError`] for
/// IO / oversize failures.
pub async fn read_frame<R>(stream: &mut R) -> Result<Option<Vec<u8>>>
where
    R: AsyncRead + Unpin,
{
    let len = match stream.read_u32().await {
        Ok(n) => n as usize,
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(WireError::from(e)),
    };

    if len > MAX_FRAME_BYTES {
        return Err(WireError::FrameTooLarge {
            len,
            max: MAX_FRAME_BYTES,
        });
    }

    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await?;
    Ok(Some(buf))
}

/// Write a length-prefixed MessagePack frame to a stream.
///
/// Flushes the stream after writing so the peer doesn't sit waiting for
/// a buffered frame. The 4-byte length is written in big-endian as
/// produced by [`tokio::io::AsyncWriteExt::write_u32`].
pub async fn write_frame<W>(stream: &mut W, data: &[u8]) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    stream.write_u32(data.len() as u32).await?;
    stream.write_all(data).await?;
    stream.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    #[tokio::test]
    async fn frame_roundtrip() {
        let (mut a, mut b) = duplex(1024);
        let payload = b"hello, ctxd";
        write_frame(&mut a, payload).await.expect("write");
        let got = read_frame(&mut b).await.expect("read").expect("some");
        assert_eq!(got.as_slice(), payload);
    }

    #[tokio::test]
    async fn frame_eof_returns_none() {
        let (a, mut b) = duplex(1024);
        // Drop the writer half — read_frame should observe a clean EOF
        // at the frame boundary and return Ok(None).
        drop(a);
        let got = read_frame(&mut b).await.expect("read");
        assert!(got.is_none(), "expected None on clean EOF, got {got:?}");
    }

    #[tokio::test]
    async fn frame_too_large_rejected() {
        let (mut a, mut b) = duplex(1024);
        // Synthesize a header claiming MAX_FRAME_BYTES + 1 — the reader
        // must reject before allocating the buffer. Note: u32 is wide
        // enough to express this, but only if MAX_FRAME_BYTES + 1 fits;
        // it does (16 MiB + 1 << u32::MAX).
        a.write_u32((MAX_FRAME_BYTES + 1) as u32)
            .await
            .expect("write header");
        let err = read_frame(&mut b).await.expect_err("must reject");
        match err {
            WireError::FrameTooLarge { len, max } => {
                assert_eq!(len, MAX_FRAME_BYTES + 1);
                assert_eq!(max, MAX_FRAME_BYTES);
            }
            other => panic!("expected FrameTooLarge, got {other:?}"),
        }
    }
}
