//! macOS launchd backend.
//!
//! Writes `~/Library/LaunchAgents/<label>.plist` and drives the
//! lifecycle via `launchctl bootstrap` / `launchctl bootout`.
//!
//! ## Why hand-roll the plist
//!
//! The `plist` crate would let us serialise a struct, but our
//! schema is tiny and Apple's plist parser is forgiving. Hand-rolled
//! XML keeps the dependency footprint small and makes the generated
//! file inspect-friendly with `cat`.
//!
//! ## launchctl modern surface
//!
//! macOS 10.10+ uses `bootstrap` / `bootout` rather than the legacy
//! `load` / `unload`. The `gui/$UID` domain is the per-user
//! launchd context — what `LaunchAgents/` plists target by default.
//!
//! ## KeepAlive semantics
//!
//! `KeepAlive = { SuccessfulExit = false }` means launchd respawns
//! the daemon if it exits with a non-zero status, but lets a
//! graceful `exit(0)` stick. This is critical for `ctxd offboard` —
//! we need the service to actually stay stopped after `bootout` and
//! not be respawned on the next exit.

use super::{
    atomic_write, read_if_exists, InstallAction, InstallReport, ServiceBackend, ServiceSpec,
    ServiceStatus,
};
use anyhow::{Context, Result};
use std::path::PathBuf;
use std::process::Command;

/// launchd-backed [`ServiceBackend`].
pub struct Launchd {
    label: String,
    plist_path: PathBuf,
}

impl Launchd {
    /// Construct a launchd backend for `label` (e.g. `dev.ctxd.daemon`).
    /// Looks up `~/Library/LaunchAgents/<label>.plist`.
    pub fn new(label: String) -> Result<Self> {
        let home = std::env::var("HOME").context("$HOME not set")?;
        let plist_path = PathBuf::from(home)
            .join("Library/LaunchAgents")
            .join(format!("{label}.plist"));
        Ok(Self { label, plist_path })
    }

    /// `gui/$UID/<label>` — the launchctl service identifier for
    /// per-user agents.
    fn service_id(&self) -> String {
        // SAFETY: getuid never fails.
        let uid = unsafe { libc::getuid() };
        format!("gui/{uid}/{}", self.label)
    }

    /// `gui/$UID` — the launchctl bootstrap target for this user.
    fn user_domain(&self) -> String {
        let uid = unsafe { libc::getuid() };
        format!("gui/{uid}")
    }
}

impl ServiceBackend for Launchd {
    fn install(&self, spec: &ServiceSpec) -> Result<InstallReport> {
        let new_plist = render_plist(spec);
        let action = match read_if_exists(&self.plist_path)? {
            Some(existing) if existing == new_plist.as_bytes() => InstallAction::Unchanged,
            Some(_) => {
                atomic_write(&self.plist_path, new_plist.as_bytes())?;
                InstallAction::Updated
            }
            None => {
                atomic_write(&self.plist_path, new_plist.as_bytes())?;
                InstallAction::Created
            }
        };
        Ok(InstallReport {
            action,
            unit_path: self.plist_path.clone(),
        })
    }

    fn start(&self) -> Result<()> {
        // `bootstrap` loads + starts the plist. Idempotent: if the
        // service is already loaded, launchctl exits non-zero with
        // EALREADY (errno 17) — we tolerate that.
        let out = Command::new("launchctl")
            .arg("bootstrap")
            .arg(self.user_domain())
            .arg(&self.plist_path)
            .output()
            .context("failed to spawn launchctl bootstrap")?;
        if out.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        // launchctl prints "service already loaded" or similar on
        // re-bootstrap. Accept any wording matching that intent.
        if stderr.contains("already")
            || stderr.contains("Service is enabled")
            || stderr.contains("Bootstrap failed: 5: Input/output error")
        {
            return Ok(());
        }
        anyhow::bail!(
            "launchctl bootstrap failed:\n  stderr: {stderr}\n  stdout: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }

    fn stop(&self) -> Result<()> {
        // `bootout` unloads + stops. Idempotent: a not-loaded service
        // returns non-zero with "Could not find service" — tolerated.
        let out = Command::new("launchctl")
            .arg("bootout")
            .arg(self.service_id())
            .output()
            .context("failed to spawn launchctl bootout")?;
        if out.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        if stderr.contains("Could not find") || stderr.contains("No such") {
            return Ok(());
        }
        anyhow::bail!(
            "launchctl bootout failed:\n  stderr: {stderr}\n  stdout: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }

    fn uninstall(&self) -> Result<()> {
        // Best-effort stop first. We tolerate stop errors here so a
        // half-broken state doesn't block plist removal.
        let _ = self.stop();
        match std::fs::remove_file(&self.plist_path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(anyhow::Error::new(e).context(format!(
                "failed to remove plist {plist:?}",
                plist = self.plist_path
            ))),
        }
    }

    fn status(&self) -> Result<ServiceStatus> {
        if !self.plist_path.exists() {
            return Ok(ServiceStatus::NotInstalled);
        }
        // `launchctl print gui/$UID/<label>` exits 0 when the service
        // is loaded (whether running or not), and 113 when not loaded.
        // The "state = running" line in the verbose dump tells us
        // running-vs-stopped. Older OSes use "pid =".
        let out = Command::new("launchctl")
            .arg("print")
            .arg(self.service_id())
            .output()
            .context("failed to spawn launchctl print")?;
        if !out.status.success() {
            return Ok(ServiceStatus::Stopped);
        }
        let dump = String::from_utf8_lossy(&out.stdout);
        let pid = dump.lines().find_map(|l| {
            let l = l.trim();
            l.strip_prefix("pid = ")
                .and_then(|rest| rest.split_whitespace().next())
                .and_then(|s| s.parse::<u32>().ok())
        });
        let running = pid.is_some()
            || dump
                .lines()
                .any(|l| l.trim().starts_with("state = running"));
        if running {
            Ok(ServiceStatus::Running { pid })
        } else {
            Ok(ServiceStatus::Stopped)
        }
    }

    fn unit_path(&self) -> PathBuf {
        self.plist_path.clone()
    }

    fn name(&self) -> &'static str {
        "launchd"
    }
}

/// Render the launchd plist for `spec`. Returns a complete XML
/// document ready to be written to disk.
///
/// Public-but-`pub(super)` for testability — tests can inspect the
/// generated XML without going through `install()`.
pub(super) fn render_plist(spec: &ServiceSpec) -> String {
    let mut args_xml = String::new();
    args_xml.push_str(&xml_str(&spec.binary.to_string_lossy()));
    args_xml.push_str("            <string>serve</string>\n");
    for a in &spec.args {
        args_xml.push_str(&xml_str(a));
    }
    let stdout_log = spec.log_dir.join("stdout.log");
    let stderr_log = spec.log_dir.join("stderr.log");
    let run_at_load = if spec.at_login { "true" } else { "false" };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
{args}    </array>
    <key>RunAtLoad</key>
    <{run_at_load}/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>WorkingDirectory</key>
    <string>{wd}</string>
    <key>StandardOutPath</key>
    <string>{out}</string>
    <key>StandardErrorPath</key>
    <string>{err}</string>
    <key>EnvironmentVariables</key>
    <dict>
        <key>HOME</key>
        <string>{home}</string>
    </dict>
</dict>
</plist>
"#,
        label = xml_escape(&spec.label),
        args = args_xml,
        run_at_load = run_at_load,
        wd = xml_escape(&spec.working_dir.to_string_lossy()),
        out = xml_escape(&stdout_log.to_string_lossy()),
        err = xml_escape(&stderr_log.to_string_lossy()),
        home = xml_escape(&std::env::var("HOME").unwrap_or_default()),
    )
}

/// Render one `<string>...</string>` element with proper indentation.
fn xml_str(s: &str) -> String {
    format!("            <string>{}</string>\n", xml_escape(s))
}

/// Minimal XML escape for the values that show up in our plist
/// (paths, label). Apple's parser tolerates these escapes but the
/// raw `&`, `<`, `>` would fail strict parsing.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_spec() -> ServiceSpec {
        ServiceSpec {
            binary: "/opt/homebrew/bin/ctxd".into(),
            args: vec![
                "--db".into(),
                "/Users/me/Library/Application Support/ctxd/ctxd.db".into(),
                "--bind".into(),
                "127.0.0.1:7777".into(),
            ],
            at_login: false,
            working_dir: "/Users/me/Library/Application Support/ctxd".into(),
            log_dir: "/Users/me/Library/Logs/ctxd".into(),
            label: "dev.ctxd.daemon".into(),
        }
    }

    #[test]
    fn plist_contains_required_keys() {
        let xml = render_plist(&fixture_spec());
        assert!(xml.starts_with("<?xml version=\"1.0\""));
        assert!(xml.contains("<key>Label</key>"));
        assert!(xml.contains("<string>dev.ctxd.daemon</string>"));
        assert!(xml.contains("<key>ProgramArguments</key>"));
        assert!(xml.contains("<key>RunAtLoad</key>"));
        assert!(xml.contains("<false/>"));
        assert!(xml.contains("<key>KeepAlive</key>"));
        assert!(xml.contains("<key>SuccessfulExit</key>"));
        assert!(xml.contains("<key>WorkingDirectory</key>"));
        assert!(xml.contains("<key>StandardOutPath</key>"));
        assert!(xml.contains("<key>StandardErrorPath</key>"));
    }

    #[test]
    fn plist_at_login_flag_flips_run_at_load() {
        let mut spec = fixture_spec();
        spec.at_login = true;
        let xml = render_plist(&spec);
        // Expect <true/> right after <key>RunAtLoad</key> when at_login.
        let idx = xml.find("<key>RunAtLoad</key>").unwrap();
        let after = &xml[idx..];
        assert!(
            after.contains("<true/>"),
            "expected <true/> after RunAtLoad: {after}"
        );
    }

    #[test]
    fn plist_includes_binary_and_serve_subcommand() {
        let xml = render_plist(&fixture_spec());
        assert!(xml.contains("<string>/opt/homebrew/bin/ctxd</string>"));
        // The subcommand is hard-wired to `serve`.
        assert!(xml.contains("<string>serve</string>"));
        assert!(xml.contains("<string>--db</string>"));
        assert!(xml.contains("<string>--bind</string>"));
        assert!(xml.contains("<string>127.0.0.1:7777</string>"));
    }

    #[test]
    fn plist_escapes_xml_specials_in_paths() {
        let mut spec = fixture_spec();
        spec.binary = "/path/with <special> & 'chars'".into();
        let xml = render_plist(&spec);
        assert!(xml.contains("&lt;special&gt;"));
        assert!(xml.contains("&amp;"));
        assert!(xml.contains("&apos;chars&apos;"));
        assert!(!xml.contains("'chars'"), "raw apostrophes must be escaped");
    }

    #[test]
    fn plist_keepalive_only_on_failure() {
        // A successful exit (e.g. user ran `ctxd offboard`) must NOT
        // trigger respawn. KeepAlive { SuccessfulExit = false } means
        // "respawn unless we exited successfully."
        let xml = render_plist(&fixture_spec());
        let ka = xml.find("<key>KeepAlive</key>").unwrap();
        let snippet = &xml[ka..ka + 200];
        assert!(snippet.contains("<key>SuccessfulExit</key>"));
        // The boolean value following SuccessfulExit must be false.
        let se = snippet.find("<key>SuccessfulExit</key>").unwrap();
        let after_se = &snippet[se..];
        assert!(after_se.contains("<false/>"));
    }

    #[test]
    fn install_creates_then_unchanged_then_updated() {
        let dir = tempfile::tempdir().unwrap();
        let plist_path = dir.path().join("test.plist");
        // Construct directly instead of via `new()` so we don't
        // depend on $HOME — keeps the test hermetic.
        let backend = Launchd {
            label: "dev.ctxd.daemon".to_string(),
            plist_path: plist_path.clone(),
        };
        let mut spec = fixture_spec();
        spec.working_dir = dir.path().to_path_buf();
        spec.log_dir = dir.path().to_path_buf();

        let r1 = backend.install(&spec).unwrap();
        assert_eq!(r1.action, InstallAction::Created);
        assert!(plist_path.exists());

        let r2 = backend.install(&spec).unwrap();
        assert_eq!(r2.action, InstallAction::Unchanged);

        spec.binary = "/different/binary".into();
        let r3 = backend.install(&spec).unwrap();
        assert_eq!(r3.action, InstallAction::Updated);
        let on_disk = std::fs::read_to_string(&plist_path).unwrap();
        assert!(on_disk.contains("/different/binary"));
    }

    #[test]
    fn status_reports_not_installed_when_plist_missing() {
        let dir = tempfile::tempdir().unwrap();
        let backend = Launchd {
            label: "dev.ctxd.daemon".into(),
            plist_path: dir.path().join("missing.plist"),
        };
        assert_eq!(backend.status().unwrap(), ServiceStatus::NotInstalled);
    }

    #[test]
    fn uninstall_is_noop_when_not_installed() {
        let dir = tempfile::tempdir().unwrap();
        let backend = Launchd {
            label: "dev.ctxd.daemon".into(),
            plist_path: dir.path().join("missing.plist"),
        };
        // Should not error even though there's nothing to remove.
        backend
            .uninstall()
            .expect("uninstall on clean system is a no-op");
    }

    #[test]
    fn xml_escape_handles_ampersand_first() {
        // Order matters: replace `&` first, otherwise we'd double-escape.
        assert_eq!(xml_escape("a&b"), "a&amp;b");
        assert_eq!(xml_escape("<&>"), "&lt;&amp;&gt;");
    }
}
