//! Cross-platform "the daemon is ready" signal.
//!
//! Two channels, both fired right after the HTTP listener has bound
//! and is accepting connections:
//!
//! 1. **`READY=1` to systemd** via `sd_notify` (Linux only). The
//!    systemd-user unit installed by `ctxd onboard` declares
//!    `Type=notify`, which means systemd holds the unit in
//!    "activating" state until this signal fires. Without it, systemd
//!    treats the unit as ready the moment fork returns and `ctxd
//!    onboard` step `service-start` races the HTTP bind.
//!
//! 2. **A stable marker line on stderr** in the form
//!    `ctxd-ready: http://127.0.0.1:7777`. macOS launchd has no notify
//!    protocol, so we fall back to a parseable stderr line that
//!    `ctxd onboard --service-start` can grep its way through. Also
//!    useful for humans running `ctxd serve` in a foreground terminal
//!    — no log-level filtering can hide it (it's a plain `eprintln!`,
//!    not a `tracing!` call).
//!
//! Calling `signal_ready` more than once is harmless: systemd
//! tolerates duplicate `READY=1`, and a duplicate stderr line is just
//! noise.

/// Notify supervisors that the daemon has finished startup.
///
/// `admin_url` is the URL the daemon is now listening on (typically
/// `http://127.0.0.1:7777`). `wire_url` is the wire-protocol bind, if
/// any — included in the marker line so external tooling can scrape
/// both addresses from one place.
pub fn signal_ready(admin_url: &str, wire_url: Option<&str>) {
    sd_notify_ready();
    let marker = match wire_url {
        Some(w) => format!("ctxd-ready: {admin_url} (wire {w})"),
        None => format!("ctxd-ready: {admin_url}"),
    };
    // Plain stderr, never filtered. The structured log fires too via
    // the caller; this exists for consumers that don't want to parse
    // tracing output (launchd polling, the skill, integration tests).
    eprintln!("{marker}");
}

#[cfg(target_os = "linux")]
fn sd_notify_ready() {
    // `sd_notify` is a thin wrapper over the AF_UNIX socket protocol
    // systemd uses for notification. When `NOTIFY_SOCKET` is not in
    // the environment (e.g. when ctxd is run outside systemd), the
    // call is a no-op — exactly what we want.
    if let Err(e) = sd_notify::notify(false, &[sd_notify::NotifyState::Ready]) {
        // We log at warn but do not propagate: sd_notify failure
        // means we won't transition systemd's unit out of activating,
        // but the daemon itself is fine.
        tracing::warn!(error = %e, "sd_notify(READY=1) failed; systemd may not transition unit to active");
    }
}

#[cfg(not(target_os = "linux"))]
fn sd_notify_ready() {
    // No-op on macOS / Windows / BSDs.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_ready_does_not_panic_with_admin_only() {
        // If sd_notify is unavailable (no NOTIFY_SOCKET), this should
        // gracefully degrade. We can't easily capture stderr in
        // tests, so this is a smoke test that asserts no panic.
        signal_ready("http://127.0.0.1:7777", None);
    }

    #[test]
    fn signal_ready_does_not_panic_with_wire() {
        signal_ready("http://127.0.0.1:7777", Some("tcp://127.0.0.1:7778"));
    }
}
