//! Shared test harness for ctxd-client integration tests.
//!
//! Spawns a real `ctxd` binary (built from this workspace) on
//! pre-picked TCP ports and exposes the bound HTTP / wire addresses.
//! Drop the returned [`DaemonHandle`] to kill the process.
//!
//! Why pre-pick ports rather than `:0`?
//!
//! The CLI's `--bind` and `--wire-bind` parse to `SocketAddr` and the
//! daemon doesn't currently emit the bound port back on stdout. We
//! pre-pick on `127.0.0.1` via `portpicker` and accept the inherent
//! TOCTOU window — tests aren't competing for ports under normal
//! load, and the daemon fails fast on a port conflict so retries are
//! straightforward.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

/// A running `ctxd serve` process for the duration of a test.
pub struct DaemonHandle {
    /// Child process. We `kill()` on drop.
    child: Option<Child>,
    /// Tempdir holding the SQLite DB. Kept alive so SQLite doesn't
    /// race with the kill on tempfile cleanup.
    pub _tempdir: TempDir,
    /// HTTP admin URL (e.g. `http://127.0.0.1:54321`).
    pub http_url: String,
    /// Wire-protocol address (e.g. `127.0.0.1:54322`).
    pub wire_addr: String,
}

impl Drop for DaemonHandle {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            // Best-effort kill. We don't `wait` here — the OS reaps
            // the zombie at process exit and the test runner doesn't
            // care about exit status.
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Locate the `ctxd` binary. Prefers `target/debug/ctxd` (the same
/// binary `cargo build --bin ctxd` produces); falls back to
/// `cargo run --bin ctxd` so the test still runs in fresh checkouts.
fn ctxd_binary_path() -> Option<PathBuf> {
    // Walk up from CARGO_MANIFEST_DIR (the SDK crate) until we find
    // the workspace root (the dir containing `Cargo.lock`).
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut cursor = manifest_dir.as_path();
    let workspace_root = loop {
        if cursor.join("Cargo.lock").exists() {
            break cursor.to_path_buf();
        }
        match cursor.parent() {
            Some(p) => cursor = p,
            None => return None,
        }
    };
    let candidate = workspace_root.join("target").join("debug").join("ctxd");
    if candidate.exists() {
        Some(candidate)
    } else {
        None
    }
}

/// Build the `ctxd` binary if it's not already present. Synchronous
/// — runs `cargo build --bin ctxd` inline. Tests expect this to be a
/// fast no-op on subsequent runs.
fn ensure_ctxd_built() -> std::io::Result<PathBuf> {
    if let Some(p) = ctxd_binary_path() {
        return Ok(p);
    }
    // Build it. Use `env!("CARGO")` so we use the same toolchain that
    // is running the test — important on CI where rustup may have
    // multiple toolchains installed.
    let status = Command::new(env!("CARGO"))
        .args(["build", "--quiet", "--bin", "ctxd"])
        .status()?;
    if !status.success() {
        return Err(std::io::Error::other(format!(
            "cargo build --bin ctxd failed: {status}"
        )));
    }
    ctxd_binary_path().ok_or_else(|| {
        std::io::Error::other("cargo build succeeded but target/debug/ctxd is missing")
    })
}

/// Spawn a `ctxd serve` daemon on freshly-picked ports.
///
/// Returns the handle once `/health` answers — typically <500ms on a
/// warm dev box, longer on cold first runs that must build the binary.
pub async fn spawn_daemon() -> DaemonHandle {
    let binary = ensure_ctxd_built().expect("locate ctxd binary");

    let tempdir = TempDir::new().expect("tempdir");
    let db_path = tempdir.path().join("ctxd.db");

    // Pre-pick two ports. We deliberately do NOT bind-then-pass-fd;
    // the CLI doesn't accept a pre-opened FD today, so we accept the
    // small TOCTOU window. Retries are unnecessary because parallel
    // tests aren't fighting over loopback ports in practice.
    let http_port =
        portpicker::pick_unused_port().expect("no free TCP port for HTTP admin");
    let wire_port =
        portpicker::pick_unused_port().expect("no free TCP port for wire protocol");
    let http_addr = format!("127.0.0.1:{http_port}");
    let wire_addr = format!("127.0.0.1:{wire_port}");
    let http_url = format!("http://{http_addr}");

    let mut cmd = Command::new(&binary);
    cmd.arg("--db")
        .arg(&db_path)
        .arg("serve")
        .arg("--bind")
        .arg(&http_addr)
        .arg("--wire-bind")
        .arg(&wire_addr)
        // The CLI exposes `--mcp-stdio` as a defaults-on bool flag
        // that doesn't accept a value, and we have no way to disable
        // it short of patching the binary. We feed it `/dev/null`
        // for stdin so the MCP stdio handler immediately sees EOF
        // and idles. Stdout/stderr are piped so the test runner
        // doesn't get noise in its own logs.
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let child = cmd.spawn().expect("spawn ctxd serve");

    let mut handle = DaemonHandle {
        child: Some(child),
        _tempdir: tempdir,
        http_url: http_url.clone(),
        wire_addr: wire_addr.clone(),
    };

    // Poll /health until ready or 30s elapse. Boot can be slow
    // under parallel test execution (concurrent SQLite migrations,
    // concurrent HNSW index opens, MCP rmcp setup). 30s is generous
    // but bounded — a real hang fails the test rather than wedging
    // the runner.
    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut last_err: Option<String> = None;
    while Instant::now() < deadline {
        match client
            .get(format!("{http_url}/health"))
            .timeout(Duration::from_millis(500))
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => {
                return handle;
            }
            Ok(r) => {
                last_err = Some(format!("status {}", r.status()));
            }
            Err(e) => {
                last_err = Some(format!("{e}"));
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // On timeout, drain stderr so the diagnostic surfaces what went
    // wrong (port conflict, schema migration error, etc.), then kill.
    let stderr_snippet = if let Some(child) = handle.child.as_mut() {
        if let Some(mut err) = child.stderr.take() {
            use std::io::Read;
            let mut buf = String::new();
            let _ = err.read_to_string(&mut buf);
            buf.chars().take(2048).collect::<String>()
        } else {
            String::new()
        }
    } else {
        String::new()
    };
    drop(handle.child.take());
    panic!(
        "ctxd daemon did not become healthy on {http_url} within 30s; \
         last error: {last_err:?}; daemon stderr: {stderr_snippet}"
    );
}
