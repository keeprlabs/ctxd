//! `ctxd doctor` — diagnostic check suite.
//!
//! The doctor is two things at once:
//!
//! 1. **A standalone command** users run when something feels off.
//!    Prints a green/red checklist plus per-failure remediation.
//! 2. **The closing step (`step 7`) of `ctxd onboard`.** The pipeline
//!    runs the same checks and emits each as a [`SkillMessage::Step`]
//!    update so the skill can render a tally line at the end.
//!
//! Each check is a pure async function returning a [`Check`] —
//! they're sequenced, not parallel, because (a) the count is small
//! and (b) some checks transitively depend on earlier ones (you
//! can't validate the cap-files until storage-healthy passes).
//!
//! The phase 1B drop wires the three checks that don't depend on
//! later phases. The remaining checks are stubbed as `Skipped` with
//! a "phase X" reason; they grow real implementations as 1C–3B land.
//! Returning `Skipped` (rather than not appearing) lets the skill
//! render a stable checklist regardless of which phases have shipped.

use crate::onboard::caps::{self, ClientId};
use crate::onboard::clients::{ClaudeCode, ClaudeDesktop, ClientSpec, ClientWriter, Codex};
use crate::onboard::paths;
use crate::onboard::service::{self, ServiceStatus};
use crate::pidfile::{self, DaemonState};
use ctxd_cap::CapEngine;
use ctxd_store::EventStore;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Result of one diagnostic check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Check {
    /// Stable identifier for the check (e.g. `"daemon-running"`).
    pub name: String,
    /// Pass / warn / fail / skip.
    pub status: CheckStatus,
    /// One-line hint for the user when status is not `Ok`. Often a
    /// shell command they can copy-paste.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
    /// Structured details (e.g. paths, versions, error messages).
    /// Skill renderers may surface these; the human renderer
    /// suppresses unless the check failed.
    #[serde(default, skip_serializing_if = "is_null_value")]
    pub detail: serde_json::Value,
}

fn is_null_value(v: &serde_json::Value) -> bool {
    v.is_null()
}

/// Outcome of one check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CheckStatus {
    /// Check passed.
    Ok,
    /// Anomaly worth surfacing but not blocking.
    Warn,
    /// Check failed.
    Failed,
    /// Check was intentionally skipped (precondition unmet,
    /// dependency not yet shipped, `--skip-*` flag set).
    Skipped,
}

/// Tally of a check run, used by the skill protocol's `Done`
/// outcome and the human renderer's footer.
#[derive(Debug, Clone, Default)]
pub struct Summary {
    pub total: u32,
    pub ok: u32,
    pub warnings: u32,
    pub failed: u32,
    pub skipped: u32,
}

impl Summary {
    /// Roll up a slice of checks into per-bucket counts.
    pub fn from_checks(checks: &[Check]) -> Self {
        let mut s = Self::default();
        for c in checks {
            s.total += 1;
            match c.status {
                CheckStatus::Ok => s.ok += 1,
                CheckStatus::Warn => s.warnings += 1,
                CheckStatus::Failed => s.failed += 1,
                CheckStatus::Skipped => s.skipped += 1,
            }
        }
        s
    }

    /// `true` if every check passed (warnings and skips are
    /// tolerated; only `Failed` flips the bit).
    pub fn all_ok(&self) -> bool {
        self.failed == 0
    }
}

/// Run every diagnostic check against the daemon backed by `db_path`.
///
/// `db_path` is the path passed via `ctxd --db ...` (or the default
/// `ctxd.db`). Checks that need to talk to a running daemon resolve
/// it via the pidfile alongside this DB.
pub async fn run(db_path: &Path) -> Vec<Check> {
    let mut checks = Vec::with_capacity(9);
    checks.push(check_daemon_running(db_path).await);
    checks.push(check_storage_healthy(db_path).await);
    checks.push(check_events_present(db_path).await);
    // Stubs for not-yet-wired checks. They report `Skipped` so the
    // skill renders a stable checklist; each will gain a real impl
    // in its phase. Listed in pipeline order to match the protocol's
    // `step` taxonomy.
    checks.push(check_service_installed());
    checks.push(check_client_config(
        db_path,
        &ClaudeDesktop,
        "claude-desktop-config",
    ));
    checks.push(check_client_config(
        db_path,
        &ClaudeCode,
        "claude-code-config",
    ));
    checks.push(check_client_config(db_path, &Codex, "codex-config"));
    checks.push(check_caps_valid(db_path).await);
    checks.push(check_adapters());
    checks
}

/// `daemon-running` — pidfile points at a live process whose `/health`
/// answers 2xx. Failure means the user has no daemon and most other
/// checks will skip.
async fn check_daemon_running(db_path: &Path) -> Check {
    match pidfile::detect(db_path).await {
        DaemonState::Running(pf) => {
            let uptime_s = (chrono::Utc::now() - pf.started_at).num_seconds().max(0);
            Check {
                name: "daemon-running".into(),
                status: CheckStatus::Ok,
                remediation: None,
                detail: serde_json::json!({
                    "pid": pf.pid,
                    "admin_url": format!("http://{}", pf.admin_bind),
                    "version": pf.version,
                    "uptime_s": uptime_s,
                }),
            }
        }
        DaemonState::Unresponsive(pf) => Check {
            name: "daemon-running".into(),
            status: CheckStatus::Warn,
            remediation: Some(format!(
                "daemon at pid {} owns the pidfile but /health is not responding; \
                 consider `kill {}` and restarting via `ctxd serve`",
                pf.pid, pf.pid
            )),
            detail: serde_json::json!({
                "pid": pf.pid,
                "admin_bind": pf.admin_bind,
                "started_at": pf.started_at,
            }),
        },
        DaemonState::Stale(pf) => Check {
            name: "daemon-running".into(),
            status: CheckStatus::Warn,
            remediation: Some(format!(
                "stale pidfile from prior daemon (pid {} no longer running); \
                 will be overwritten on next `ctxd serve`",
                pf.pid
            )),
            detail: serde_json::json!({"stale_pid": pf.pid}),
        },
        DaemonState::NotRunning => Check {
            name: "daemon-running".into(),
            status: CheckStatus::Failed,
            remediation: Some("start the daemon with `ctxd serve` (or run `ctxd onboard`)".into()),
            detail: serde_json::Value::Null,
        },
    }
}

/// `storage-healthy` — the SQLite file at `db_path` opens cleanly.
/// Skipped if the file doesn't exist yet (pre-onboard).
async fn check_storage_healthy(db_path: &Path) -> Check {
    if !db_path.exists() {
        return Check {
            name: "storage-healthy".into(),
            status: CheckStatus::Skipped,
            remediation: Some(format!(
                "no DB at {} yet — first `ctxd serve` will create it",
                db_path.to_string_lossy()
            )),
            detail: serde_json::Value::Null,
        };
    }
    match EventStore::open(db_path).await {
        Ok(_store) => Check {
            name: "storage-healthy".into(),
            status: CheckStatus::Ok,
            remediation: None,
            detail: serde_json::json!({"path": db_path.to_string_lossy()}),
        },
        Err(e) => Check {
            name: "storage-healthy".into(),
            status: CheckStatus::Failed,
            remediation: Some(
                "the SQLite DB cannot be opened — check file permissions, disk \
                 space, or restore from a backup"
                    .into(),
            ),
            detail: serde_json::json!({"error": e.to_string()}),
        },
    }
}

/// `events-present` — the log contains at least one event. Failure
/// after a successful onboard would mean seeding didn't run; on a
/// brand-new install we skip rather than fail.
async fn check_events_present(db_path: &Path) -> Check {
    if !db_path.exists() {
        return Check {
            name: "events-present".into(),
            status: CheckStatus::Skipped,
            remediation: Some(
                "no DB yet — onboard step seed-subjects writes 3 events to /me/**".into(),
            ),
            detail: serde_json::Value::Null,
        };
    }
    let store = match EventStore::open(db_path).await {
        Ok(s) => s,
        Err(_) => {
            return Check {
                name: "events-present".into(),
                status: CheckStatus::Skipped,
                remediation: Some("storage-healthy must pass first".into()),
                detail: serde_json::Value::Null,
            };
        }
    };
    match store.event_count().await {
        Ok(n) if n > 0 => Check {
            name: "events-present".into(),
            status: CheckStatus::Ok,
            remediation: None,
            detail: serde_json::json!({"event_count": n}),
        },
        Ok(_) => Check {
            name: "events-present".into(),
            status: CheckStatus::Warn,
            remediation: Some(
                "the log is empty — run `ctxd onboard` (step seed-subjects) to populate /me/**"
                    .into(),
            ),
            detail: serde_json::json!({"event_count": 0}),
        },
        Err(e) => Check {
            name: "events-present".into(),
            status: CheckStatus::Failed,
            remediation: Some("could not count events; storage may be corrupt".into()),
            detail: serde_json::json!({"error": e.to_string()}),
        },
    }
}

/// `service-installed` — launchd plist / systemd unit is on disk.
/// Reports `skipped` on Windows / unsupported platforms; never
/// `failed` (a missing service is the user's choice — they may run
/// `ctxd serve` in a foreground terminal).
fn check_service_installed() -> Check {
    if !service::is_supported() {
        return Check {
            name: "service-installed".into(),
            status: CheckStatus::Skipped,
            remediation: Some(
                "service install not supported on this OS yet (v0.4 ships macOS + Linux); \
                 run `ctxd serve` in a foreground terminal as a workaround"
                    .into(),
            ),
            detail: serde_json::Value::Null,
        };
    }
    let backend = match service::detect_backend(paths::SERVICE_LABEL) {
        Ok(b) => b,
        Err(e) => {
            return Check {
                name: "service-installed".into(),
                status: CheckStatus::Failed,
                remediation: Some("could not initialise service backend; check $HOME".into()),
                detail: serde_json::json!({"error": e.to_string()}),
            };
        }
    };
    let unit_path = backend.unit_path();
    match backend.status() {
        Ok(ServiceStatus::Running { pid }) => Check {
            name: "service-installed".into(),
            status: CheckStatus::Ok,
            remediation: None,
            detail: serde_json::json!({
                "backend": backend.name(),
                "unit_path": unit_path.to_string_lossy(),
                "state": "running",
                "pid": pid,
            }),
        },
        Ok(ServiceStatus::Stopped) => Check {
            name: "service-installed".into(),
            status: CheckStatus::Warn,
            remediation: Some(format!(
                "service unit installed but not running — start with `ctxd onboard --only service-start` \
                 or `{}`",
                if backend.name() == "launchd" {
                    "launchctl bootstrap gui/$UID <plist>"
                } else {
                    "systemctl --user start ctxd"
                }
            )),
            detail: serde_json::json!({
                "backend": backend.name(),
                "unit_path": unit_path.to_string_lossy(),
                "state": "stopped",
            }),
        },
        Ok(ServiceStatus::NotInstalled) => Check {
            name: "service-installed".into(),
            status: CheckStatus::Failed,
            remediation: Some(
                "no service installed — run `ctxd onboard --only service-install,service-start` \
                 to install and start"
                    .into(),
            ),
            detail: serde_json::json!({
                "backend": backend.name(),
                "expected_unit_path": unit_path.to_string_lossy(),
            }),
        },
        Err(e) => Check {
            name: "service-installed".into(),
            status: CheckStatus::Failed,
            remediation: Some(format!("service backend ({}) error", backend.name())),
            detail: serde_json::json!({"error": e.to_string()}),
        },
    }
}

/// `<client>-config` — the client's settings file contains an
/// `mcpServers.ctxd` entry that matches what onboard would write
/// today (i.e. same binary, same DB, same cap-file pointer).
fn check_client_config(db_path: &Path, writer: &dyn ClientWriter, name: &str) -> Check {
    let binary = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            return Check {
                name: name.into(),
                status: CheckStatus::Failed,
                remediation: Some("could not resolve current ctxd binary".into()),
                detail: serde_json::json!({"error": e.to_string()}),
            };
        }
    };
    let spec = ClientSpec {
        binary,
        db_path: db_path.to_path_buf(),
        with_hooks: false,
    };
    let path = match writer.config_path() {
        Ok(p) => p,
        Err(e) => {
            return Check {
                name: name.into(),
                status: CheckStatus::Failed,
                remediation: Some("could not resolve client config path".into()),
                detail: serde_json::json!({"error": e.to_string()}),
            };
        }
    };
    // Codex is special: its writer reports verify=true when the
    // snippet file we wrote is on disk, but it doesn't represent the
    // user actually pasting it. Surface as `Warn` (with manual-pending
    // wording in remediation) rather than `Ok` so the user knows.
    let is_codex = matches!(writer.client_id(), ClientId::Codex);
    match writer.verify(&spec) {
        Ok(true) => Check {
            name: name.into(),
            status: if is_codex {
                CheckStatus::Warn
            } else {
                CheckStatus::Ok
            },
            remediation: if is_codex {
                Some(format!(
                    "Codex requires a manual paste — see {}",
                    path.to_string_lossy()
                ))
            } else {
                None
            },
            detail: serde_json::json!({"path": path.to_string_lossy()}),
        },
        Ok(false) => Check {
            name: name.into(),
            status: if writer.detect() {
                CheckStatus::Failed
            } else {
                CheckStatus::Skipped
            },
            remediation: Some(format!(
                "{} not configured — run `ctxd onboard --only configure-clients`",
                writer.client_id().display()
            )),
            detail: serde_json::json!({"path": path.to_string_lossy(), "detected": writer.detect()}),
        },
        Err(e) => Check {
            name: name.into(),
            status: CheckStatus::Failed,
            remediation: Some("could not verify client config".into()),
            detail: serde_json::json!({"error": e.to_string()}),
        },
    }
}

/// `caps-valid` — every minted cap-file decodes against the root
/// key, has not expired, and grants the operation it should. Skipped
/// when no caps directory exists yet (pre-onboard).
async fn check_caps_valid(db_path: &Path) -> Check {
    let caps_dir = match paths::caps_dir() {
        Ok(p) => p,
        Err(e) => {
            return Check {
                name: "caps-valid".into(),
                status: CheckStatus::Failed,
                remediation: Some("could not resolve caps_dir; check $HOME".into()),
                detail: serde_json::json!({"error": e.to_string()}),
            };
        }
    };
    if !caps_dir.exists() {
        return Check {
            name: "caps-valid".into(),
            status: CheckStatus::Skipped,
            remediation: Some(
                "no cap files yet — run `ctxd onboard --only mint-capabilities` to mint".into(),
            ),
            detail: serde_json::Value::Null,
        };
    }
    if !db_path.exists() {
        return Check {
            name: "caps-valid".into(),
            status: CheckStatus::Skipped,
            remediation: Some("no DB yet — caps cannot be verified without the root key".into()),
            detail: serde_json::Value::Null,
        };
    }
    // Open store + load root key. We can verify caps without a
    // running daemon — verification is purely cryptographic.
    let store = match EventStore::open(db_path).await {
        Ok(s) => s,
        Err(e) => {
            return Check {
                name: "caps-valid".into(),
                status: CheckStatus::Failed,
                remediation: Some("could not open store to verify caps".into()),
                detail: serde_json::json!({"error": e.to_string()}),
            };
        }
    };
    let root_bytes = match store.get_metadata("root_key").await {
        Ok(Some(b)) => b,
        Ok(None) => {
            return Check {
                name: "caps-valid".into(),
                status: CheckStatus::Skipped,
                remediation: Some(
                    "no root_key persisted yet — first `ctxd serve` or onboard mint creates it"
                        .into(),
                ),
                detail: serde_json::Value::Null,
            };
        }
        Err(e) => {
            return Check {
                name: "caps-valid".into(),
                status: CheckStatus::Failed,
                remediation: Some("could not read root_key from store".into()),
                detail: serde_json::json!({"error": e.to_string()}),
            };
        }
    };
    let cap_engine = match CapEngine::from_private_key(&root_bytes) {
        Ok(e) => e,
        Err(e) => {
            return Check {
                name: "caps-valid".into(),
                status: CheckStatus::Failed,
                remediation: Some("stored root key is invalid; daemon may need re-init".into()),
                detail: serde_json::json!({"error": format!("{e}")}),
            };
        }
    };
    let clients = [
        ClientId::ClaudeDesktop,
        ClientId::ClaudeCode,
        ClientId::Codex,
        ClientId::GmailAdapter,
        ClientId::GithubAdapter,
        ClientId::FsAdapter,
    ];
    let mut reports = Vec::new();
    let mut failures = 0;
    let mut missing = 0;
    for c in clients {
        let r = match caps::verify_persisted(&cap_engine, c) {
            Ok(r) => r,
            Err(e) => {
                failures += 1;
                reports.push(serde_json::json!({
                    "client": c.slug(),
                    "ok": false,
                    "error": e.to_string(),
                }));
                continue;
            }
        };
        if !r.present {
            missing += 1;
        } else if !r.decodes || !r.verifies_default_op {
            failures += 1;
        }
        reports.push(serde_json::json!({
            "client": r.client.slug(),
            "present": r.present,
            "decodes": r.decodes,
            "verifies_default_op": r.verifies_default_op,
            "error": r.error,
        }));
    }
    let status = if failures > 0 {
        CheckStatus::Failed
    } else if missing > 0 {
        CheckStatus::Warn
    } else {
        CheckStatus::Ok
    };
    let remediation = match status {
        CheckStatus::Failed => {
            Some("re-mint caps via `ctxd onboard --only mint-capabilities`".into())
        }
        CheckStatus::Warn => Some(format!(
            "{missing} cap file(s) missing — `ctxd onboard --only mint-capabilities` to fill"
        )),
        _ => None,
    };
    Check {
        name: "caps-valid".into(),
        status,
        remediation,
        detail: serde_json::json!({
            "checked": reports.len(),
            "missing": missing,
            "failures": failures,
            "reports": reports,
        }),
    }
}

/// `adapters` — read skills.toml and surface what's enabled and
/// (for adapters whose spawn is wired) whether the supporting
/// state (paths exist, token files present) is in place.
fn check_adapters() -> Check {
    let manifest = match crate::onboard::skills_toml::read() {
        Ok(m) => m,
        Err(e) => {
            return Check {
                name: "adapters".into(),
                status: CheckStatus::Failed,
                remediation: Some(
                    "could not read skills.toml; run `ctxd onboard --only configure-adapters`"
                        .into(),
                ),
                detail: serde_json::json!({"error": e.to_string()}),
            };
        }
    };

    let mut detail = serde_json::Map::new();
    let mut any_enabled = false;
    let mut any_failed = false;

    if let Some(fs) = manifest.fs {
        if fs.enabled && !fs.paths.is_empty() {
            any_enabled = true;
            // Verify each path exists.
            let missing: Vec<_> = fs
                .paths
                .iter()
                .filter(|p| !p.exists())
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            if missing.is_empty() {
                detail.insert(
                    "fs".into(),
                    serde_json::json!({
                        "status": "enabled",
                        "paths": fs.paths,
                    }),
                );
            } else {
                any_failed = true;
                detail.insert(
                    "fs".into(),
                    serde_json::json!({
                        "status": "missing-paths",
                        "missing": missing,
                    }),
                );
            }
        } else {
            detail.insert("fs".into(), serde_json::json!({"status": "disabled"}));
        }
    } else {
        detail.insert("fs".into(), serde_json::json!({"status": "not-configured"}));
    }

    if let Some(g) = manifest.gmail {
        detail.insert(
            "gmail".into(),
            serde_json::json!({
                "status": if g.enabled { "manual-pending" } else { "disabled" },
                "token_file": g.token_file.to_string_lossy(),
            }),
        );
    } else {
        detail.insert(
            "gmail".into(),
            serde_json::json!({"status": "not-configured"}),
        );
    }
    if let Some(g) = manifest.github {
        detail.insert(
            "github".into(),
            serde_json::json!({
                "status": if g.enabled { "manual-pending" } else { "disabled" },
                "token_file": g.token_file.to_string_lossy(),
            }),
        );
    } else {
        detail.insert(
            "github".into(),
            serde_json::json!({"status": "not-configured"}),
        );
    }

    let status = if any_failed {
        CheckStatus::Failed
    } else if any_enabled {
        CheckStatus::Ok
    } else {
        CheckStatus::Skipped
    };
    let remediation = match status {
        CheckStatus::Failed => Some(
            "one or more configured adapter paths are missing — fix the path or re-run \
             `ctxd onboard --only configure-adapters`"
                .into(),
        ),
        CheckStatus::Skipped => Some(
            "no adapters enabled — run `ctxd onboard --fs ~/Documents/notes` to start \
             watching a directory"
                .into(),
        ),
        _ => None,
    };
    Check {
        name: "adapters".into(),
        status,
        remediation,
        detail: serde_json::Value::Object(detail),
    }
}

/// Render `checks` as a human-readable terminal output. Returns
/// `true` if every check passed (or was skipped/warned non-fatally).
pub fn render_human(checks: &[Check]) -> bool {
    for c in checks {
        let marker = match c.status {
            CheckStatus::Ok => "  ✓",
            CheckStatus::Warn => "  !",
            CheckStatus::Failed => "  ✗",
            CheckStatus::Skipped => "  ↷",
        };
        println!("{marker} {}", c.name);
        if let Some(r) = &c.remediation {
            if c.status != CheckStatus::Ok {
                println!("      {r}");
            }
        }
        if c.status == CheckStatus::Failed && !c.detail.is_null() {
            println!("      detail: {}", c.detail);
        }
    }
    let s = Summary::from_checks(checks);
    println!();
    println!(
        "  {}/{} ok, {} warning, {} failed, {} skipped",
        s.ok, s.total, s.warnings, s.failed, s.skipped
    );
    s.all_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn doctor_run_returns_all_known_checks() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("ctxd.db");
        let checks = run(&db).await;
        // Stable check inventory for v0.4. Skill UIs depend on this list.
        let names: Vec<&str> = checks.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "daemon-running",
                "storage-healthy",
                "events-present",
                "service-installed",
                "claude-desktop-config",
                "claude-code-config",
                "codex-config",
                "caps-valid",
                "adapters",
            ]
        );
    }

    #[tokio::test]
    async fn fresh_install_doctor_failures_carry_remediations() {
        // On a brand-new install we expect *some* failures (at minimum
        // daemon-running; on supported OSes also service-installed if
        // no plist/unit is on disk). The strict count varies by host
        // because service-installed reads $HOME/Library/LaunchAgents
        // (mac) or ~/.config/systemd/user (linux). The contract we
        // DO pin: every failure carries a remediation string — that
        // is the whole point of the doctor.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("ctxd.db");
        let checks = run(&db).await;
        for c in &checks {
            if matches!(c.status, CheckStatus::Failed) {
                assert!(
                    c.remediation.is_some(),
                    "failed check `{}` has no remediation",
                    c.name
                );
            }
        }
        // daemon-running must always fail on a fresh install (no
        // pidfile, no listener).
        let daemon = checks.iter().find(|c| c.name == "daemon-running").unwrap();
        assert_eq!(daemon.status, CheckStatus::Failed);
        // The fresh tempdir scenario has at least *some* skips —
        // storage-healthy (no DB yet), events-present, caps-valid,
        // and the still-stubbed `adapters` check. Don't pin the
        // exact count; future phases land more real checks.
        let summary = Summary::from_checks(&checks);
        assert!(
            summary.skipped >= 1,
            "expected ≥1 skipped check, got {} ({summary:?})",
            summary.skipped
        );
    }

    #[tokio::test]
    async fn storage_healthy_passes_on_real_db() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("ctxd.db");
        // Open then drop to materialise the SQLite file.
        let _ = ctxd_store::EventStore::open(&db).await.unwrap();
        let check = check_storage_healthy(&db).await;
        assert_eq!(check.status, CheckStatus::Ok, "got {check:?}");
    }

    #[tokio::test]
    async fn events_present_warns_on_empty_db() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("ctxd.db");
        let _ = ctxd_store::EventStore::open(&db).await.unwrap();
        let check = check_events_present(&db).await;
        assert_eq!(check.status, CheckStatus::Warn, "got {check:?}");
    }

    #[test]
    fn summary_rolls_up_correctly() {
        let checks = vec![
            Check {
                name: "a".into(),
                status: CheckStatus::Ok,
                remediation: None,
                detail: serde_json::Value::Null,
            },
            Check {
                name: "b".into(),
                status: CheckStatus::Failed,
                remediation: None,
                detail: serde_json::Value::Null,
            },
            Check {
                name: "c".into(),
                status: CheckStatus::Skipped,
                remediation: None,
                detail: serde_json::Value::Null,
            },
            Check {
                name: "d".into(),
                status: CheckStatus::Warn,
                remediation: None,
                detail: serde_json::Value::Null,
            },
        ];
        let s = Summary::from_checks(&checks);
        assert_eq!(s.total, 4);
        assert_eq!(s.ok, 1);
        assert_eq!(s.warnings, 1);
        assert_eq!(s.failed, 1);
        assert_eq!(s.skipped, 1);
        assert!(!s.all_ok());
    }

    #[test]
    fn all_ok_tolerates_warn_and_skip() {
        let s = Summary {
            total: 3,
            ok: 1,
            warnings: 1,
            failed: 0,
            skipped: 1,
        };
        assert!(s.all_ok());
    }

    #[test]
    fn check_serialises_with_kebab_case_status() {
        let c = Check {
            name: "x".into(),
            status: CheckStatus::Ok,
            remediation: None,
            detail: serde_json::Value::Null,
        };
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(v["status"], "ok");
        assert!(v.get("remediation").is_none());
        assert!(v.get("detail").is_none());
    }
}
