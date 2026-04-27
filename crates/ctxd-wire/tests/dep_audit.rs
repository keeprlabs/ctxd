//! Dependency-boundary audit.
//!
//! `ctxd-wire` is the **lean leaf** SDK clients depend on. Its
//! transitive dependency tree must not include any of the daemon-side
//! heavy hitters (axum, rmcp, sqlx) or any other ctxd workspace crate
//! that owns server-side state (`ctxd-store`, `ctxd-cap`, `ctxd-mcp`,
//! `ctxd-http`, `ctxd-cli`).
//!
//! This test shells out to `cargo tree` on the host toolchain and
//! greps for forbidden crate names. It runs against the actual lock
//! file so a sloppy `git push` that re-introduces, say, `axum` via a
//! "while I'm here" change gets caught at CI time before it ships.
//!
//! If `cargo` is somehow not on PATH (extremely unusual in the dev
//! environment, impossible in CI), the test prints a SKIP message and
//! returns — failing here would be noise, not signal.

use std::process::Command;

const FORBIDDEN: &[&str] = &[
    // Daemon-side HTTP / MCP / SQL stacks.
    "axum",
    "rmcp",
    "sqlx",
    // Other ctxd crates that depend on the daemon-side stack.
    "ctxd-store",
    "ctxd-store-core",
    "ctxd-store-sqlite",
    "ctxd-store-postgres",
    "ctxd-store-duckobj",
    "ctxd-cap",
    "ctxd-mcp",
    "ctxd-http",
    "ctxd-cli",
    "ctxd-embed",
];

#[test]
fn ctxd_wire_has_no_heavy_deps() {
    let manifest = format!("{}/Cargo.toml", env!("CARGO_MANIFEST_DIR"));
    let output = match Command::new(env!("CARGO"))
        .args([
            "tree",
            "--manifest-path",
            &manifest,
            "-p",
            "ctxd-wire",
            "--edges",
            "no-dev",
            "--prefix",
            "none",
        ])
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            eprintln!("SKIP: cargo tree not runnable ({e})");
            return;
        }
    };

    assert!(
        output.status.success(),
        "cargo tree failed: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut hits: Vec<String> = Vec::new();
    for line in stdout.lines() {
        // Each line begins with the crate name followed by a space
        // and the version; with `--prefix none` there's no graph
        // glyph to strip.
        let name = line.split_whitespace().next().unwrap_or("");
        if FORBIDDEN.contains(&name) {
            hits.push(line.to_string());
        }
    }

    assert!(
        hits.is_empty(),
        "ctxd-wire pulled in forbidden dependencies — the lean-leaf invariant is broken:\n{}\n\nFull tree:\n{}",
        hits.join("\n"),
        stdout,
    );
}
