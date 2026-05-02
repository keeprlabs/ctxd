//! `--skill-mode` JSON protocol — the versioned contract between
//! `ctxd onboard` and any external front door (Claude Code skill, web
//! installer, IDE plugins).
//!
//! ## Wire format
//!
//! Newline-delimited JSON ("JSON Lines"): each [`SkillMessage`] is
//! serialised as a single line on stdout, terminated by `\n`. Logs and
//! tracing remain on stderr (see `init_tracing` in `main.rs`), so the
//! skill can capture stdout cleanly without filtering.
//!
//! Every message carries a `protocol` integer. The skill MUST refuse to
//! interpret messages whose `protocol` does not match the version it
//! was built against — bumping the version is how we signal a
//! breaking change.
//!
//! ## One-way for v0.4
//!
//! In v0.4 the protocol is strictly one-way: ctxd writes, the skill
//! reads. The skill encodes user choices as CLI flags before invoking
//! ctxd (e.g. `--gmail=skip`, `--fs=~/Documents/notes`). For
//! out-of-band actions like OAuth device flow, ctxd emits
//! [`SkillMessage::Notice`] with a URL + code; the skill displays it
//! and ctxd polls for completion in the background. A bidirectional
//! stdin response channel may land in v0.5 if a real prompt-shaped
//! interaction shows up; today every interactive question can be
//! pre-answered as a flag, so we keep the contract simple.
//!
//! ## Output modes
//!
//! [`Emitter`] supports three output modes selected at the top of
//! `ctxd onboard`:
//!
//! * [`OutputMode::Skill`] — JSON lines on stdout. The mode `--skill-mode` selects.
//! * [`OutputMode::Human`] — friendly, coloured progress for direct CLI use.
//! * [`OutputMode::Null`] — discard everything (used by tests).
//!
//! Higher-level orchestration code never branches on the mode — it
//! calls helper methods like [`Emitter::step_started`] /
//! [`Emitter::step_ok`] which dispatch internally.

use serde::{Deserialize, Serialize};
use std::io::Write;

/// Current protocol version. Bump on any breaking change to the
/// [`SkillMessage`] shape — including renames, removed fields, or
/// changed semantics. Additive changes (new variants, new optional
/// fields) do not require a bump.
pub const PROTOCOL_VERSION: u8 = 1;

/// One message on the `--skill-mode` wire.
///
/// `kind` discriminates the variant so the skill can switch on it
/// without having to look at which fields are present.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SkillMessage {
    /// A step in the onboarding pipeline transitioned. Most messages
    /// are this variant. `detail` is variant-by-step structured data
    /// the skill can render (e.g. for `service-install` it carries the
    /// platform + plist path; for `mint-capabilities` it carries the
    /// list of clients tokens were minted for).
    Step {
        protocol: u8,
        step: StepName,
        status: StepStatus,
        #[serde(default, skip_serializing_if = "is_null_value")]
        detail: serde_json::Value,
    },

    /// A user-visible notice that does not expect a response. Used
    /// for OAuth device-flow prompts, ambient warnings, and similar.
    /// The skill renders this directly to the user; ctxd continues
    /// without waiting.
    Notice {
        protocol: u8,
        id: String,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        action_url: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        action_code: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        expires_at: Option<chrono::DateTime<chrono::Utc>>,
    },

    /// Diagnostic log line. Skills typically render `info` and above;
    /// `debug` is for skill developers chasing problems.
    Log {
        protocol: u8,
        level: LogLevel,
        message: String,
    },

    /// Terminal success. The pipeline finished and the system is in
    /// the state described by [`Outcome`].
    Done { protocol: u8, outcome: Outcome },

    /// Terminal failure. The pipeline aborted at `step` with `message`;
    /// `remediation` is a one-line hint the skill can surface (e.g.
    /// `"ctxd onboard --only configure-clients"`).
    Error {
        protocol: u8,
        step: StepName,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        remediation: Option<String>,
    },
}

/// The seven steps of `ctxd onboard`, plus `snapshot` (step 0,
/// pre-flight) and `doctor` (step 7, verify). The order in this enum
/// matches the order steps fire in a full run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StepName {
    /// Pre-flight scan: detect running daemons, existing client config,
    /// non-canonical binaries. Snapshots current state so `offboard`
    /// can restore it.
    Snapshot,
    /// Install the daemon as a user-scope service (launchd plist /
    /// systemd-user unit).
    ServiceInstall,
    /// Start the service and wait for `/health` to return 200.
    ServiceStart,
    /// Write MCP server entries for Claude Desktop, Claude Code, Codex.
    ConfigureClients,
    /// Mint per-client capability tokens scoped to `/me/**` (or
    /// narrower with `--strict-scopes`), persist as `0600` files.
    MintCapabilities,
    /// Populate `/me/profile`, `/me/preferences`, `/me/about` so a
    /// fresh onboard yields non-empty answers.
    SeedSubjects,
    /// Walk OAuth / PAT flows for opt-in adapters (gmail, github, fs).
    ConfigureAdapters,
    /// Run the diagnostic check suite and report.
    Doctor,
}

impl StepName {
    /// Stable string slug for the step. Matches the `serde` rename.
    /// Useful for logging and remediation strings.
    pub fn slug(self) -> &'static str {
        match self {
            StepName::Snapshot => "snapshot",
            StepName::ServiceInstall => "service-install",
            StepName::ServiceStart => "service-start",
            StepName::ConfigureClients => "configure-clients",
            StepName::MintCapabilities => "mint-capabilities",
            StepName::SeedSubjects => "seed-subjects",
            StepName::ConfigureAdapters => "configure-adapters",
            StepName::Doctor => "doctor",
        }
    }
}

/// Status of a step transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StepStatus {
    /// Step is beginning. Emitted once per step.
    Started,
    /// Step completed successfully.
    Ok,
    /// Step was intentionally skipped (user declined, already done,
    /// `--skip-*` flag set).
    Skipped,
    /// Step partially completed but requires the user to do something
    /// out-of-band before it can be marked ok. Used today for Codex
    /// configuration: ctxd prints the snippet, the user pastes it, the
    /// next `ctxd doctor` run promotes the status to ok.
    ManualPending,
    /// Step failed. The pipeline stops; an [`SkillMessage::Error`]
    /// follows.
    Failed,
}

/// Severity level for [`SkillMessage::Log`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

/// Final summary attached to [`SkillMessage::Done`]. Lists what the
/// pipeline actually accomplished (independent of what was requested).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Outcome {
    /// `true` if every required step reported `ok`. `false` if any
    /// step ended `manual-pending` or `failed`.
    pub onboarded: bool,
    /// Slugs of clients whose configuration was written or verified
    /// (e.g. `["claude-desktop", "claude-code"]`).
    pub clients_configured: Vec<String>,
    /// Slugs of adapters that ended `ok` (e.g. `["gmail", "fs"]`).
    pub adapters_enabled: Vec<String>,
    /// Per-bucket counts from the closing doctor run.
    pub doctor: DoctorSummary,
}

/// Per-bucket counts from a doctor run. Skills typically render this
/// as a tally line: `9/10 ok, 1 warning`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DoctorSummary {
    /// Total number of checks performed.
    pub total: u32,
    /// Checks that passed.
    pub ok: u32,
    /// Checks that produced a warning (non-fatal anomaly).
    pub warnings: u32,
    /// Checks that failed.
    pub failed: u32,
}

/// Output transport selected at top-level by the `--skill-mode` flag
/// (or interactive default).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    /// Newline-delimited JSON to stdout. The format the skill speaks.
    Skill,
    /// Friendly coloured progress for direct CLI use.
    Human,
    /// Discard everything. Used by tests and `--quiet` paths.
    Null,
}

/// Emitter funnels every onboarding message through one place so
/// callers don't have to think about output formatting. Cheap to
/// clone — internally just an `OutputMode`.
#[derive(Debug, Clone, Copy)]
pub struct Emitter {
    mode: OutputMode,
}

impl Emitter {
    /// Construct an emitter for the given mode.
    pub fn new(mode: OutputMode) -> Self {
        Self { mode }
    }

    /// Currently active mode.
    pub fn mode(&self) -> OutputMode {
        self.mode
    }

    /// Emit a fully-formed message.
    pub fn emit(&self, msg: &SkillMessage) {
        match self.mode {
            OutputMode::Skill => emit_jsonl(msg),
            OutputMode::Human => emit_human(msg),
            OutputMode::Null => {}
        }
    }

    /// Convenience: Started transition for `step`. Matches the more
    /// common usage of "I am beginning step X."
    pub fn step_started(&self, step: StepName) {
        self.emit(&SkillMessage::Step {
            protocol: PROTOCOL_VERSION,
            step,
            status: StepStatus::Started,
            detail: serde_json::Value::Null,
        });
    }

    /// Convenience: Ok transition for `step` with structured detail.
    pub fn step_ok(&self, step: StepName, detail: serde_json::Value) {
        self.emit(&SkillMessage::Step {
            protocol: PROTOCOL_VERSION,
            step,
            status: StepStatus::Ok,
            detail,
        });
    }

    /// Convenience: Skipped transition for `step` with a reason.
    pub fn step_skipped(&self, step: StepName, reason: impl Into<String>) {
        self.emit(&SkillMessage::Step {
            protocol: PROTOCOL_VERSION,
            step,
            status: StepStatus::Skipped,
            detail: serde_json::json!({ "reason": reason.into() }),
        });
    }

    /// Convenience: ManualPending transition for `step` with the
    /// instructions the user needs to follow.
    pub fn step_manual_pending(&self, step: StepName, instructions: impl Into<String>) {
        self.emit(&SkillMessage::Step {
            protocol: PROTOCOL_VERSION,
            step,
            status: StepStatus::ManualPending,
            detail: serde_json::json!({ "instructions": instructions.into() }),
        });
    }

    /// Convenience: Failed transition for `step` (no detail).
    pub fn step_failed(&self, step: StepName) {
        self.emit(&SkillMessage::Step {
            protocol: PROTOCOL_VERSION,
            step,
            status: StepStatus::Failed,
            detail: serde_json::Value::Null,
        });
    }

    /// Emit a notice. Use this when the user must see a message
    /// during a step (e.g. an OAuth device URL), not afterward.
    pub fn notice(&self, id: impl Into<String>, message: impl Into<String>) {
        self.emit(&SkillMessage::Notice {
            protocol: PROTOCOL_VERSION,
            id: id.into(),
            message: message.into(),
            action_url: None,
            action_code: None,
            expires_at: None,
        });
    }

    /// Emit a notice with an actionable URL + code (OAuth device flow).
    pub fn notice_with_action(
        &self,
        id: impl Into<String>,
        message: impl Into<String>,
        action_url: impl Into<String>,
        action_code: impl Into<String>,
        expires_at: Option<chrono::DateTime<chrono::Utc>>,
    ) {
        self.emit(&SkillMessage::Notice {
            protocol: PROTOCOL_VERSION,
            id: id.into(),
            message: message.into(),
            action_url: Some(action_url.into()),
            action_code: Some(action_code.into()),
            expires_at,
        });
    }

    /// Diagnostic log message.
    pub fn log(&self, level: LogLevel, message: impl Into<String>) {
        self.emit(&SkillMessage::Log {
            protocol: PROTOCOL_VERSION,
            level,
            message: message.into(),
        });
    }

    /// Terminal success.
    pub fn done(&self, outcome: Outcome) {
        self.emit(&SkillMessage::Done {
            protocol: PROTOCOL_VERSION,
            outcome,
        });
    }

    /// Terminal failure.
    pub fn error(&self, step: StepName, message: impl Into<String>, remediation: Option<String>) {
        self.emit(&SkillMessage::Error {
            protocol: PROTOCOL_VERSION,
            step,
            message: message.into(),
            remediation,
        });
    }
}

fn is_null_value(v: &serde_json::Value) -> bool {
    v.is_null()
}

fn emit_jsonl(msg: &SkillMessage) {
    // Failures here are unrecoverable (broken stdout pipe means the
    // skill is gone) and we'd rather lose a status line than panic.
    let line = match serde_json::to_string(msg) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "failed to serialize skill message");
            return;
        }
    };
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

fn emit_human(msg: &SkillMessage) {
    // Human mode is intentionally minimal in v0.4 — pretty colours can
    // come later. The format here is "what a developer running ctxd
    // onboard in their terminal sees", optimised for grep-ability and
    // information density rather than visual flair.
    match msg {
        SkillMessage::Step {
            step,
            status,
            detail,
            ..
        } => {
            let marker = match status {
                StepStatus::Started => "  …",
                StepStatus::Ok => "  ✓",
                StepStatus::Skipped => "  ↷",
                StepStatus::ManualPending => "  !",
                StepStatus::Failed => "  ✗",
            };
            let extra = if detail.is_null() {
                String::new()
            } else if let Some(reason) = detail.get("reason").and_then(|v| v.as_str()) {
                format!("  ({reason})")
            } else if let Some(instr) = detail.get("instructions").and_then(|v| v.as_str()) {
                format!("\n      {instr}")
            } else {
                String::new()
            };
            println!("{marker} {}{}", step.slug(), extra);
        }
        SkillMessage::Notice {
            message,
            action_url,
            action_code,
            ..
        } => {
            println!("  ℹ {message}");
            if let (Some(url), Some(code)) = (action_url, action_code) {
                println!("      visit {url} and enter code: {code}");
            }
        }
        SkillMessage::Log { level, message, .. } => {
            let prefix = match level {
                LogLevel::Debug => "    debug",
                LogLevel::Info => "    info ",
                LogLevel::Warn => "    warn ",
                LogLevel::Error => "    error",
            };
            println!("{prefix} {message}");
        }
        SkillMessage::Done { outcome, .. } => {
            let DoctorSummary {
                total,
                ok,
                warnings,
                failed,
            } = outcome.doctor;
            println!();
            println!(
                "  done — {} client(s) configured, {} adapter(s) enabled",
                outcome.clients_configured.len(),
                outcome.adapters_enabled.len()
            );
            println!("  doctor: {ok}/{total} ok, {warnings} warn, {failed} fail");
        }
        SkillMessage::Error {
            step,
            message,
            remediation,
            ..
        } => {
            eprintln!("  ✗ {} — {message}", step.slug());
            if let Some(r) = remediation {
                eprintln!("    fix: {r}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! Stability tests: the JSON shape is the binary↔skill contract.
    //! Renames or field removals here are breaking changes that
    //! require bumping `PROTOCOL_VERSION`. These tests are designed to
    //! fail loudly when that happens so the change can't be silent.

    use super::*;
    use serde_json::json;

    #[test]
    fn protocol_version_is_one() {
        assert_eq!(PROTOCOL_VERSION, 1);
    }

    #[test]
    fn step_started_serialises_with_protocol_field() {
        let msg = SkillMessage::Step {
            protocol: PROTOCOL_VERSION,
            step: StepName::ServiceInstall,
            status: StepStatus::Started,
            detail: serde_json::Value::Null,
        };
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&msg).unwrap()).unwrap();
        assert_eq!(v["kind"], "step");
        assert_eq!(v["protocol"], 1);
        assert_eq!(v["step"], "service-install");
        assert_eq!(v["status"], "started");
        // Null `detail` is omitted by the custom `skip_serializing_if`.
        assert!(v.get("detail").map(|d| d.is_null()).unwrap_or(true));
    }

    #[test]
    fn step_ok_includes_detail() {
        let msg = SkillMessage::Step {
            protocol: PROTOCOL_VERSION,
            step: StepName::ConfigureClients,
            status: StepStatus::Ok,
            detail: json!({ "clients": { "claude-desktop": "configured" } }),
        };
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&msg).unwrap()).unwrap();
        assert_eq!(v["status"], "ok");
        assert_eq!(v["detail"]["clients"]["claude-desktop"], "configured");
    }

    #[test]
    fn notice_with_action_round_trips() {
        let now = chrono::Utc::now();
        let msg = SkillMessage::Notice {
            protocol: PROTOCOL_VERSION,
            id: "gmail-oauth".to_string(),
            message: "visit the URL and enter the code".to_string(),
            action_url: Some("https://google.com/device".to_string()),
            action_code: Some("ABCD-EFGH".to_string()),
            expires_at: Some(now),
        };
        let line = serde_json::to_string(&msg).unwrap();
        let parsed: SkillMessage = serde_json::from_str(&line).unwrap();
        match parsed {
            SkillMessage::Notice {
                id,
                action_url,
                action_code,
                ..
            } => {
                assert_eq!(id, "gmail-oauth");
                assert_eq!(action_url.as_deref(), Some("https://google.com/device"));
                assert_eq!(action_code.as_deref(), Some("ABCD-EFGH"));
            }
            _ => panic!("expected Notice, got {parsed:?}"),
        }
    }

    #[test]
    fn notice_omits_optional_fields_when_unset() {
        let msg = SkillMessage::Notice {
            protocol: PROTOCOL_VERSION,
            id: "info".to_string(),
            message: "hi".to_string(),
            action_url: None,
            action_code: None,
            expires_at: None,
        };
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&msg).unwrap()).unwrap();
        assert_eq!(v["kind"], "notice");
        assert!(
            v.get("action_url").is_none(),
            "action_url should be omitted"
        );
        assert!(
            v.get("action_code").is_none(),
            "action_code should be omitted"
        );
        assert!(
            v.get("expires_at").is_none(),
            "expires_at should be omitted"
        );
    }

    #[test]
    fn done_includes_outcome() {
        let outcome = Outcome {
            onboarded: true,
            clients_configured: vec!["claude-desktop".to_string()],
            adapters_enabled: vec!["fs".to_string()],
            doctor: DoctorSummary {
                total: 8,
                ok: 8,
                warnings: 0,
                failed: 0,
            },
        };
        let msg = SkillMessage::Done {
            protocol: PROTOCOL_VERSION,
            outcome,
        };
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&msg).unwrap()).unwrap();
        assert_eq!(v["kind"], "done");
        assert_eq!(v["outcome"]["onboarded"], true);
        assert_eq!(v["outcome"]["doctor"]["ok"], 8);
        assert_eq!(v["outcome"]["clients_configured"][0], "claude-desktop");
    }

    #[test]
    fn error_omits_remediation_when_unset() {
        let msg = SkillMessage::Error {
            protocol: PROTOCOL_VERSION,
            step: StepName::ServiceStart,
            message: "port in use".to_string(),
            remediation: None,
        };
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&msg).unwrap()).unwrap();
        assert_eq!(v["kind"], "error");
        assert_eq!(v["step"], "service-start");
        assert!(v.get("remediation").is_none());
    }

    #[test]
    fn step_slugs_are_kebab_case_and_stable() {
        // Pinned strings — the skill team can rely on these. Any
        // change here is a protocol-breaking change.
        assert_eq!(StepName::Snapshot.slug(), "snapshot");
        assert_eq!(StepName::ServiceInstall.slug(), "service-install");
        assert_eq!(StepName::ServiceStart.slug(), "service-start");
        assert_eq!(StepName::ConfigureClients.slug(), "configure-clients");
        assert_eq!(StepName::MintCapabilities.slug(), "mint-capabilities");
        assert_eq!(StepName::SeedSubjects.slug(), "seed-subjects");
        assert_eq!(StepName::ConfigureAdapters.slug(), "configure-adapters");
        assert_eq!(StepName::Doctor.slug(), "doctor");
    }

    #[test]
    fn null_emitter_is_silent() {
        let e = Emitter::new(OutputMode::Null);
        // Should not panic and not write anything (caller verifies
        // stdout is untouched in integration tests). Here we just
        // exercise every helper to catch panics.
        e.step_started(StepName::Snapshot);
        e.step_ok(StepName::Snapshot, json!({}));
        e.step_skipped(StepName::Snapshot, "not needed");
        e.step_manual_pending(StepName::ConfigureClients, "paste this");
        e.step_failed(StepName::ServiceStart);
        e.notice("id", "hi");
        e.notice_with_action("id", "msg", "https://x", "CODE", None);
        e.log(LogLevel::Info, "log line");
        e.error(StepName::ServiceStart, "boom", Some("retry".into()));
        e.done(Outcome {
            onboarded: false,
            clients_configured: vec![],
            adapters_enabled: vec![],
            doctor: DoctorSummary::default(),
        });
    }

    #[test]
    fn step_message_round_trips_through_serde() {
        let original = SkillMessage::Step {
            protocol: PROTOCOL_VERSION,
            step: StepName::SeedSubjects,
            status: StepStatus::Ok,
            detail: json!({
                "subjects_created": ["/me/profile", "/me/preferences", "/me/about"],
                "events_written": 3
            }),
        };
        let line = serde_json::to_string(&original).unwrap();
        let parsed: SkillMessage = serde_json::from_str(&line).unwrap();
        match parsed {
            SkillMessage::Step {
                step,
                status,
                detail,
                ..
            } => {
                assert_eq!(step, StepName::SeedSubjects);
                assert_eq!(status, StepStatus::Ok);
                assert_eq!(detail["events_written"], 3);
            }
            _ => panic!("expected Step variant"),
        }
    }
}
