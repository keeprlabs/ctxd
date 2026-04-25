//! Shared retry helpers for HTTP-backed embedders.
//!
//! Both the OpenAI and Ollama embedders consult these helpers so the
//! retry policy is exactly one place. Keeping it `pub(crate)` means
//! the strategy can evolve without breaking SemVer on either backend.

use std::time::Duration;

/// Maximum number of attempts (including the first) before we give up.
///
/// We choose 4 — the first attempt plus three retries. With our
/// 250 ms / 500 ms / 1000 ms backoff that bounds total wall-clock at
/// ~1.75 s plus network latency, which is short enough to fail back
/// to the caller before they wonder if we hung.
pub(crate) const MAX_ATTEMPTS: u32 = 4;

/// Compute the delay before the next retry attempt.
///
/// `attempt` is 1-indexed (first retry is attempt 1). If the server
/// supplied a `Retry-After` value we honor it (capped at 30 s — we'd
/// rather fail loudly than block writes for minutes); otherwise we
/// fall back to exponential backoff: 250 ms, 500 ms, 1000 ms, …
pub(crate) fn backoff(attempt: u32, retry_after: Option<Duration>) -> Duration {
    if let Some(d) = retry_after {
        // Cap the server's Retry-After at 30s so a hostile or
        // misconfigured upstream can't stall ctxd.
        return d.min(Duration::from_secs(30));
    }
    let ms = 250u64.saturating_mul(1u64 << (attempt.saturating_sub(1).min(6)));
    Duration::from_millis(ms.min(8000))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_uses_retry_after_when_provided() {
        let d = backoff(1, Some(Duration::from_millis(750)));
        assert_eq!(d, Duration::from_millis(750));
    }

    #[test]
    fn backoff_caps_retry_after_at_30s() {
        let d = backoff(1, Some(Duration::from_secs(120)));
        assert_eq!(d, Duration::from_secs(30));
    }

    #[test]
    fn backoff_grows_exponentially_without_retry_after() {
        assert_eq!(backoff(1, None), Duration::from_millis(250));
        assert_eq!(backoff(2, None), Duration::from_millis(500));
        assert_eq!(backoff(3, None), Duration::from_millis(1000));
    }
}
