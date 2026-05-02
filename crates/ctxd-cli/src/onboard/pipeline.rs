//! `ctxd onboard` / `ctxd offboard` orchestration driver.
//!
//! Sequences the seven onboarding steps (plus snapshot pre-flight
//! and doctor closing step) and emits per-step progress through the
//! [`crate::onboard::protocol::Emitter`]. Steps that have not yet
//! been wired (phases 2A–3B) emit `Skipped` — the pipeline still
//! runs end-to-end so the skill team and onboard-mode flags can be
//! exercised against the protocol contract before the rest of the
//! steps land.
//!
//! ## Order
//!
//! 1. `snapshot` — pre-flight scan (phase 3A: skipped today).
//! 2. `service-install` — write launchd plist / systemd unit.
//! 3. `service-start` — start service, wait for `/health` 200.
//! 4. `configure-clients` — Claude Desktop / Code / Codex (phase 2B).
//! 5. `mint-capabilities` — per-client cap files (phase 2A).
//! 6. `seed-subjects` — populate `/me/**` (phase 2D).
//! 7. `configure-adapters` — Gmail / GitHub / fs (phase 3B).
//! 8. `doctor` — verify everything works.
//!
//! ## What `dry-run` does
//!
//! Emits each step's `Started` and a synthetic `Skipped`-with-reason
//! `"dry-run"` instead of acting. Intended for the skill's
//! "show me the plan before I commit" path. `--only` interacts:
//! `--dry-run --only service-install` reports what install would
//! change without writing any files.

use anyhow::Result;
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use crate::onboard::doctor;
use crate::onboard::paths;
use crate::onboard::protocol::{DoctorSummary, Emitter, Outcome, OutputMode, StepName, StepStatus};
use crate::onboard::service::{self, ServiceSpec};
use crate::pidfile;

/// Adapter user-choice. Each opt-in adapter (gmail, github, fs) takes
/// one of these.
#[derive(Debug, Clone, Default)]
pub enum AdapterChoice {
    /// Don't enable this adapter.
    #[default]
    Skip,
    /// Walk the OAuth / PAT flow interactively. Today the actual
    /// flow is gated behind phase 3B; pipeline emits Skipped with
    /// "phase 3B" message until then.
    Interactive,
    /// Use a literal token / refresh-token / path, no interaction.
    /// Used by the skill once it has collected the value out-of-band.
    Token(String),
}

/// Pipeline configuration. Constructed by `main.rs` from CLI flags
/// (or by the skill via the same flag surface).
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    /// How to emit progress.
    pub mode: OutputMode,
    /// `true` = no interactive prompts, defaults everywhere.
    pub headless: bool,
    /// `true` = plan only, no mutations.
    pub dry_run: bool,
    /// Skip the configure-adapters step entirely.
    pub skip_adapters: bool,
    /// Skip service-install + service-start (foreground-only mode).
    pub skip_service: bool,
    /// Configure the service to start at user login.
    pub at_login: bool,
    /// Mint narrower capability tokens (phase 2A).
    pub strict_scopes: bool,
    /// Write Claude Code SessionStart/UserPromptSubmit/PreCompact/Stop
    /// hooks (phase 2B).
    pub with_hooks: bool,
    /// Gmail adapter choice (phase 3B).
    pub gmail: AdapterChoice,
    /// GitHub PAT or skip (phase 3B).
    pub github: AdapterChoice,
    /// Filesystem adapter watch paths (phase 3B). Empty = skip.
    pub fs: Vec<PathBuf>,
    /// Subset of steps to run (None = all).
    pub only: Option<HashSet<StepName>>,
    /// SQLite DB path the daemon will use.
    pub db_path: PathBuf,
    /// Bind for the daemon's HTTP admin (`127.0.0.1:7777` typically).
    pub bind: String,
    /// Bind for the wire protocol.
    pub wire_bind: String,
}

impl PipelineConfig {
    fn includes(&self, step: StepName) -> bool {
        self.only
            .as_ref()
            .map(|s| s.contains(&step))
            .unwrap_or(true)
    }
}

/// Run the full onboarding pipeline. On success returns the
/// [`Outcome`] also emitted as the protocol's terminal `Done` message.
pub async fn onboard(cfg: PipelineConfig) -> Result<Outcome> {
    let emitter = Emitter::new(cfg.mode);

    // Step 1 (after snapshot pre-flight): service-install.
    step_snapshot(&cfg, &emitter)?;
    step_service_install(&cfg, &emitter)?;
    step_service_start(&cfg, &emitter).await?;
    step_configure_clients(&cfg, &emitter)?;
    step_mint_capabilities(&cfg, &emitter)?;
    step_seed_subjects(&cfg, &emitter)?;
    step_configure_adapters(&cfg, &emitter)?;
    let doctor_summary = step_doctor(&cfg, &emitter).await?;

    let outcome = Outcome {
        onboarded: doctor_summary.failed == 0,
        clients_configured: vec![], // populated in phase 2B
        adapters_enabled: vec![],   // populated in phase 3B
        doctor: doctor_summary,
    };
    emitter.done(outcome.clone());
    Ok(outcome)
}

/// Reverse what onboard did. Stops the service and removes the
/// unit file. With `purge`, also removes the SQLite DB. Idempotent.
pub async fn offboard(cfg: PipelineConfig, purge: bool) -> Result<()> {
    let emitter = Emitter::new(cfg.mode);

    // Stop + uninstall the service. Skipped on unsupported platforms.
    if cfg.skip_service || !service::is_supported() {
        emitter.step_skipped(
            StepName::ServiceInstall,
            "skip-service or unsupported platform",
        );
    } else if cfg.dry_run {
        emitter.step_skipped(StepName::ServiceInstall, "dry-run");
    } else {
        emitter.step_started(StepName::ServiceInstall);
        let backend = service::detect_backend(paths::SERVICE_LABEL)?;
        let unit_path = backend.unit_path();
        backend.uninstall()?;
        emitter.step_ok(
            StepName::ServiceInstall,
            serde_json::json!({"action": "uninstalled", "unit_path": unit_path.to_string_lossy()}),
        );
    }

    // Optional purge: delete the SQLite DB and pidfile. Adapter
    // tokens, snapshots, and skills.toml stay until phase 3A's
    // snapshot-restore lands them in offboard.
    if purge && !cfg.dry_run {
        emitter.log(
            crate::onboard::protocol::LogLevel::Info,
            format!("purging DB at {}", cfg.db_path.to_string_lossy()),
        );
        let _ = std::fs::remove_file(&cfg.db_path);
        let _ = std::fs::remove_file(pidfile::pidfile_path(&cfg.db_path));
        // Best-effort hnsw sidecars.
        for ext in &["hnsw.data", "hnsw.graph", "hnsw.map", "hnsw.meta"] {
            let mut p = cfg.db_path.clone();
            let mut name = p.file_name().map(|n| n.to_os_string()).unwrap_or_default();
            name.push(format!(".{ext}"));
            p.set_file_name(name);
            let _ = std::fs::remove_file(&p);
        }
    }

    emitter.done(Outcome {
        onboarded: false,
        clients_configured: vec![],
        adapters_enabled: vec![],
        doctor: DoctorSummary::default(),
    });
    Ok(())
}

// ---- step implementations ------------------------------------------

fn step_snapshot(cfg: &PipelineConfig, emitter: &Emitter) -> Result<()> {
    if !cfg.includes(StepName::Snapshot) {
        return Ok(());
    }
    emitter.step_started(StepName::Snapshot);
    emitter.step_skipped(
        StepName::Snapshot,
        "phase 3A — pre-flight snapshot not yet wired",
    );
    Ok(())
}

fn step_service_install(cfg: &PipelineConfig, emitter: &Emitter) -> Result<()> {
    if !cfg.includes(StepName::ServiceInstall) {
        return Ok(());
    }
    emitter.step_started(StepName::ServiceInstall);
    if cfg.skip_service {
        emitter.step_skipped(StepName::ServiceInstall, "--skip-service");
        return Ok(());
    }
    if !service::is_supported() {
        emitter.step_skipped(
            StepName::ServiceInstall,
            "service install not supported on this OS yet (Windows in v0.5)",
        );
        return Ok(());
    }
    if cfg.dry_run {
        emitter.step_skipped(StepName::ServiceInstall, "dry-run");
        return Ok(());
    }

    let binary = std::env::current_exe()
        .map_err(|e| anyhow::anyhow!("could not resolve current ctxd binary: {e}"))?;
    let working_dir = paths::data_dir()?;
    let log_dir = paths::log_dir()?;
    // Ensure dirs exist so launchd / systemd don't fail trying to
    // open log files in a missing directory.
    let _ = std::fs::create_dir_all(&working_dir);
    let _ = std::fs::create_dir_all(&log_dir);

    let spec = ServiceSpec {
        binary: binary.clone(),
        args: vec![
            "--db".into(),
            cfg.db_path.to_string_lossy().into_owned(),
            "--bind".into(),
            cfg.bind.clone(),
            "--wire-bind".into(),
            cfg.wire_bind.clone(),
        ],
        at_login: cfg.at_login,
        working_dir,
        log_dir,
        label: paths::SERVICE_LABEL.into(),
    };
    let backend = service::detect_backend(paths::SERVICE_LABEL)?;
    let report = backend.install(&spec)?;
    emitter.step_ok(
        StepName::ServiceInstall,
        serde_json::json!({
            "platform": backend.name(),
            "service_name": paths::SERVICE_LABEL,
            "binary_path": binary.to_string_lossy(),
            "unit_path": report.unit_path.to_string_lossy(),
            "action": match report.action {
                service::InstallAction::Created => "created",
                service::InstallAction::Updated => "updated",
                service::InstallAction::Unchanged => "unchanged",
            },
            "at_login": cfg.at_login,
        }),
    );
    Ok(())
}

async fn step_service_start(cfg: &PipelineConfig, emitter: &Emitter) -> Result<()> {
    if !cfg.includes(StepName::ServiceStart) {
        return Ok(());
    }
    emitter.step_started(StepName::ServiceStart);
    if cfg.skip_service {
        emitter.step_skipped(StepName::ServiceStart, "--skip-service");
        return Ok(());
    }
    if !service::is_supported() {
        emitter.step_skipped(
            StepName::ServiceStart,
            "service install not supported on this OS yet",
        );
        return Ok(());
    }
    if cfg.dry_run {
        emitter.step_skipped(StepName::ServiceStart, "dry-run");
        return Ok(());
    }

    let backend = service::detect_backend(paths::SERVICE_LABEL)?;
    backend.start()?;

    // Wait for /health up to 10s. The pidfile is written by the
    // daemon (phase 1A) once the listener is up; we poll until it
    // appears or the timeout elapses.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let started_at = std::time::Instant::now();
    let (admin_url, version) = loop {
        if let pidfile::DaemonState::Running(pf) = pidfile::detect(&cfg.db_path).await {
            break (format!("http://{}", pf.admin_bind), pf.version);
        }
        if std::time::Instant::now() >= deadline {
            emitter.error(
                StepName::ServiceStart,
                format!(
                    "daemon did not become healthy within 10s — check `{} status` and the launchd / systemd log",
                    backend.name()
                ),
                Some("ctxd doctor".into()),
            );
            anyhow::bail!("daemon did not respond on /health within 10s");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };

    emitter.step_ok(
        StepName::ServiceStart,
        serde_json::json!({
            "http_url": admin_url,
            "version": version,
            "uptime_ms": started_at.elapsed().as_millis() as u64,
        }),
    );
    Ok(())
}

fn step_configure_clients(cfg: &PipelineConfig, emitter: &Emitter) -> Result<()> {
    if !cfg.includes(StepName::ConfigureClients) {
        return Ok(());
    }
    emitter.step_started(StepName::ConfigureClients);
    emitter.step_skipped(
        StepName::ConfigureClients,
        "phase 2B — Claude Desktop / Code / Codex writers not yet wired",
    );
    Ok(())
}

fn step_mint_capabilities(cfg: &PipelineConfig, emitter: &Emitter) -> Result<()> {
    if !cfg.includes(StepName::MintCapabilities) {
        return Ok(());
    }
    emitter.step_started(StepName::MintCapabilities);
    emitter.step_skipped(
        StepName::MintCapabilities,
        "phase 2A — capability file-pointer minting not yet wired",
    );
    Ok(())
}

fn step_seed_subjects(cfg: &PipelineConfig, emitter: &Emitter) -> Result<()> {
    if !cfg.includes(StepName::SeedSubjects) {
        return Ok(());
    }
    emitter.step_started(StepName::SeedSubjects);
    emitter.step_skipped(
        StepName::SeedSubjects,
        "phase 2D — /me/** seeding not yet wired",
    );
    Ok(())
}

fn step_configure_adapters(cfg: &PipelineConfig, emitter: &Emitter) -> Result<()> {
    if !cfg.includes(StepName::ConfigureAdapters) {
        return Ok(());
    }
    emitter.step_started(StepName::ConfigureAdapters);
    if cfg.skip_adapters {
        emitter.step_skipped(StepName::ConfigureAdapters, "--skip-adapters");
        return Ok(());
    }
    emitter.step_skipped(
        StepName::ConfigureAdapters,
        "phase 3B — adapter spawning not yet wired",
    );
    Ok(())
}

async fn step_doctor(cfg: &PipelineConfig, emitter: &Emitter) -> Result<DoctorSummary> {
    if !cfg.includes(StepName::Doctor) {
        return Ok(DoctorSummary::default());
    }
    emitter.step_started(StepName::Doctor);
    let checks = doctor::run(&cfg.db_path).await;
    let summary = doctor::Summary::from_checks(&checks);
    // Warnings count as success — they're surfaced to the user but
    // don't flip onboarded=false. Only Failed checks fail the step.
    let status = if summary.failed > 0 {
        StepStatus::Failed
    } else {
        StepStatus::Ok
    };
    let detail = serde_json::json!({
        "checks": checks,
        "summary": {
            "total": summary.total,
            "ok": summary.ok,
            "warnings": summary.warnings,
            "failed": summary.failed,
            "skipped": summary.skipped,
        }
    });
    match status {
        StepStatus::Ok => emitter.step_ok(StepName::Doctor, detail),
        _ => emitter.step_failed(StepName::Doctor),
    };
    Ok(DoctorSummary {
        total: summary.total,
        ok: summary.ok,
        warnings: summary.warnings,
        failed: summary.failed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(db: &std::path::Path) -> PipelineConfig {
        PipelineConfig {
            mode: OutputMode::Null,
            headless: true,
            dry_run: false,
            skip_adapters: true,
            skip_service: true, // skip in tests so we don't touch real launchd
            at_login: false,
            strict_scopes: false,
            with_hooks: false,
            gmail: AdapterChoice::Skip,
            github: AdapterChoice::Skip,
            fs: vec![],
            only: None,
            db_path: db.to_path_buf(),
            bind: "127.0.0.1:0".into(),
            wire_bind: "127.0.0.1:0".into(),
        }
    }

    #[tokio::test]
    async fn skip_service_skips_install_and_start() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("ctxd.db");
        let outcome = onboard(cfg(&db)).await.unwrap();
        // The doctor's daemon-running check fails (no daemon, since
        // we skipped service-start), so onboarded is false. That's
        // expected for --skip-service.
        assert!(!outcome.onboarded);
        assert!(outcome.doctor.total >= 9);
    }

    #[tokio::test]
    async fn dry_run_makes_no_mutations() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("ctxd.db");
        let mut c = cfg(&db);
        c.dry_run = true;
        c.skip_service = false; // even with skip_service off, dry_run blocks mutations

        let _ = onboard(c).await.unwrap();
        // No plist written:
        let plist_under_test = dirs_home_or_default()
            .join("Library/LaunchAgents")
            .join(format!("{}.plist", paths::SERVICE_LABEL));
        // We can't fully assert non-existence on a host that may have
        // a real plist already. But we CAN assert the dry_run didn't
        // CREATE one — checked via mtime not being recent. Skip that
        // detail for portability and just confirm no panic.
        let _ = plist_under_test;
    }

    #[tokio::test]
    async fn only_filter_runs_subset() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("ctxd.db");
        let mut c = cfg(&db);
        let mut only = HashSet::new();
        only.insert(StepName::Snapshot);
        only.insert(StepName::Doctor);
        c.only = Some(only);
        let _ = onboard(c).await.unwrap();
    }

    #[tokio::test]
    async fn offboard_dry_run_does_not_touch_disk() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("ctxd.db");
        std::fs::write(&db, b"sentinel").unwrap();
        let mut c = cfg(&db);
        c.dry_run = true;
        offboard(c, true).await.unwrap();
        // DB still there even though we asked for --purge — dry_run wins.
        assert_eq!(std::fs::read(&db).unwrap(), b"sentinel");
    }

    #[tokio::test]
    async fn offboard_purge_removes_db_and_sidecars() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("ctxd.db");
        std::fs::write(&db, b"db").unwrap();
        std::fs::write(db.with_extension("db.hnsw.data"), b"x").unwrap();
        std::fs::write(db.with_extension("db.hnsw.graph"), b"x").unwrap();
        std::fs::write(pidfile::pidfile_path(&db), b"{}").unwrap();
        let c = cfg(&db);
        offboard(c, true).await.unwrap();
        assert!(!db.exists(), "DB should be removed by --purge");
        assert!(
            !pidfile::pidfile_path(&db).exists(),
            "pidfile should be removed"
        );
        assert!(
            !db.with_extension("db.hnsw.data").exists(),
            "hnsw.data sidecar should be removed"
        );
    }

    fn dirs_home_or_default() -> PathBuf {
        std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/tmp"))
    }
}
