//! Header parsing helpers (Link, Retry-After, ETag normalization).

use std::time::Duration;

/// Extract the `rel="next"` URL from an RFC 5988 / GitHub `Link` header.
///
/// Returns `None` if the header is missing or has no `next` link.
///
/// Example input:
/// `<https://api.github.com/x?page=2>; rel="next", <https://api.github.com/x?page=5>; rel="last"`
pub fn next_link(link_header: Option<&str>) -> Option<String> {
    let header = link_header?;
    for raw in header.split(',') {
        let part = raw.trim();
        // Each part looks like: <url>; rel="next"
        if !part.starts_with('<') {
            continue;
        }
        let close = part.find('>')?;
        let url = &part[1..close];
        let attrs = &part[close + 1..];
        // Attribute portion: `; rel="next"` (case-insensitive `rel`).
        let mut is_next = false;
        for attr in attrs.split(';') {
            let attr = attr.trim();
            let lower = attr.to_ascii_lowercase();
            if lower.starts_with("rel=") {
                let val = lower.trim_start_matches("rel=").trim_matches('"');
                if val == "next" {
                    is_next = true;
                    break;
                }
            }
        }
        if is_next {
            return Some(url.to_string());
        }
    }
    None
}

/// Parse a `Retry-After` header value into a [`Duration`].
///
/// GitHub returns this as an integer number of seconds. We accept that, plus
/// (defensively) HTTP-date is *not* supported here — a missing/garbled value
/// returns `None` and the caller falls back to its own backoff schedule.
pub fn retry_after(value: Option<&str>) -> Option<Duration> {
    let s = value?.trim();
    let secs: u64 = s.parse().ok()?;
    Some(Duration::from_secs(secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_link_extracts_next() {
        let h = "<https://api.github.com/repos/a/b/issues?page=2>; rel=\"next\", <https://api.github.com/repos/a/b/issues?page=5>; rel=\"last\"";
        assert_eq!(
            next_link(Some(h)),
            Some("https://api.github.com/repos/a/b/issues?page=2".to_string())
        );
    }

    #[test]
    fn next_link_handles_no_next() {
        let h = "<https://api.github.com/x?page=1>; rel=\"prev\", <https://api.github.com/x?page=1>; rel=\"first\"";
        assert!(next_link(Some(h)).is_none());
    }

    #[test]
    fn next_link_none_when_header_missing() {
        assert!(next_link(None).is_none());
    }

    #[test]
    fn next_link_handles_extra_spaces() {
        let h = "  <https://api.github.com/x?page=2>;  rel=\"next\"  ";
        assert_eq!(
            next_link(Some(h)),
            Some("https://api.github.com/x?page=2".to_string())
        );
    }

    #[test]
    fn retry_after_seconds() {
        assert_eq!(retry_after(Some("5")), Some(Duration::from_secs(5)));
        assert_eq!(retry_after(Some(" 30 ")), Some(Duration::from_secs(30)));
    }

    #[test]
    fn retry_after_invalid() {
        assert_eq!(retry_after(None), None);
        assert_eq!(retry_after(Some("garbage")), None);
    }
}
