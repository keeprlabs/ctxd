//! Simple in-memory rate limiter using a sliding window counter per token_id.

use std::collections::HashMap;
use std::time::Instant;

/// A simple sliding-window rate limiter keyed by token_id.
#[allow(dead_code)]
pub struct RateLimiter {
    /// Map of token_id -> (window_start, request_count).
    windows: HashMap<String, (Instant, u32)>,
    /// Window duration in seconds (always 1 second for ops-per-second).
    window_secs: u64,
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(dead_code)]
impl RateLimiter {
    /// Create a new rate limiter with a 1-second window.
    pub fn new() -> Self {
        Self {
            windows: HashMap::new(),
            window_secs: 1,
        }
    }

    /// Check if a request is allowed for the given token_id with the given
    /// rate limit (operations per second).
    ///
    /// Returns `true` if the request is allowed, `false` if rate-limited.
    #[allow(dead_code)]
    pub fn check(&mut self, token_id: &str, max_ops_per_sec: u32) -> bool {
        let now = Instant::now();

        let entry = self.windows.entry(token_id.to_string()).or_insert((now, 0));

        let elapsed = now.duration_since(entry.0).as_secs();
        if elapsed >= self.window_secs {
            // Reset window
            entry.0 = now;
            entry.1 = 1;
            true
        } else if entry.1 < max_ops_per_sec {
            entry.1 += 1;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_within_limit() {
        let mut limiter = RateLimiter::new();
        for _ in 0..10 {
            assert!(limiter.check("token-1", 10));
        }
    }

    #[test]
    fn rejects_over_limit() {
        let mut limiter = RateLimiter::new();
        for _ in 0..5 {
            assert!(limiter.check("token-1", 5));
        }
        // 6th request in same window should be rejected
        assert!(!limiter.check("token-1", 5));
    }

    #[test]
    fn separate_tokens_tracked_independently() {
        let mut limiter = RateLimiter::new();
        for _ in 0..3 {
            assert!(limiter.check("token-a", 3));
        }
        assert!(!limiter.check("token-a", 3));
        // Different token should still be allowed
        assert!(limiter.check("token-b", 3));
    }
}
