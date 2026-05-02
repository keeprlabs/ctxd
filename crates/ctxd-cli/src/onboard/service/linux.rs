//! Linux systemd-user backend.
//!
//! Writes `~/.config/systemd/user/<unit>.service` and drives the
//! lifecycle via `systemctl --user`.
//!
//! ## `Type=notify`
//!
//! The unit declares `Type=notify` so systemd holds the unit in the
//! `activating` state until ctxd calls `sd_notify(READY=1)` from
//! `crates/ctxd-cli/src/ready.rs`. This eliminates the bind/start
//! race that `Type=simple` would have — onboard's step
//! `service-start` can rely on `systemctl --user start ctxd` not
//! returning until the daemon is actually accepting on `:7777`.
//!
//! ## Unit name
//!
//! The launchd label is reverse-DNS (`dev.ctxd.daemon`) for Apple
//! convention. systemd unit names traditionally read forward
//! (`ctxd.service`). We strip the reverse-DNS prefix so
//! `systemctl --user status ctxd` reads naturally.

use super::{
    atomic_write, read_if_exists, InstallAction, InstallReport, ServiceBackend, ServiceSpec,
    ServiceStatus,
};
use anyhow::{Context, Result};
use std::path::PathBuf;
use std::process::Command;

/// systemd-user-backed [`ServiceBackend`].
pub struct Systemd {
    /// systemd unit short name without the `.service` suffix
    /// (e.g. `"ctxd"`). Derived from the label.
    unit_name: String,
    /// Full path to the unit file.
    unit_path: PathBuf,
}

impl Systemd {
    /// Construct a systemd-user backend for `label`.
    pub fn new(label: String) -> Result<Self> {
        let unit_name = Self::unit_name(&label);
        let home = std::env::var("HOME").context("$HOME not set")?;
        let unit_path = PathBuf::from(home)
            .join(".config/systemd/user")
            .join(format!("{unit_name}.service"));
        Ok(Self {
            unit_name,
            unit_path,
        })
    }

    /// Convert a reverse-DNS label like `dev.ctxd.daemon` into a
    /// systemd-friendly short name like `ctxd`. Logic: take the
    /// last non-`daemon`/`service` component, lowercased.
    pub(super) fn unit_name(label: &str) -> String {
        let parts: Vec<&str> = label.split('.').collect();
        let chosen = parts
            .iter()
            .rev()
            .find(|p| !matches!(**p, "daemon" | "service"))
            .copied()
            .unwrap_or("ctxd");
        chosen.to_ascii_lowercase()
    }

    /// Run `systemctl --user <args>` and return stdout/stderr.
    fn systemctl(&self, args: &[&str]) -> Result<std::process::Output> {
        Command::new("systemctl")
            .arg("--user")
            .args(args)
            .output()
            .context("failed to spawn systemctl --user")
    }
}

impl ServiceBackend for Systemd {
    fn install(&self, spec: &ServiceSpec) -> Result<InstallReport> {
        let new_unit = render_unit_file(spec);
        let action = match read_if_exists(&self.unit_path)? {
            Some(existing) if existing == new_unit.as_bytes() => InstallAction::Unchanged,
            Some(_) => {
                atomic_write(&self.unit_path, new_unit.as_bytes())?;
                InstallAction::Updated
            }
            None => {
                atomic_write(&self.unit_path, new_unit.as_bytes())?;
                InstallAction::Created
            }
        };
        // After writing the file, ask systemd to re-read it. If the
        // user is not in a systemd-user session (rare on modern
        // distros, but possible in containers) this fails; we keep
        // going since the file write is the durable part.
        let _ = self.systemctl(&["daemon-reload"]);
        if spec.at_login {
            // Enable creates a symlink under default.target.wants.
            let _ = self.systemctl(&["enable", &format!("{}.service", self.unit_name)]);
        } else {
            // If a previous install enabled at-login and this one
            // didn't, undo it. Idempotent.
            let _ = self.systemctl(&["disable", &format!("{}.service", self.unit_name)]);
        }
        Ok(InstallReport {
            action,
            unit_path: self.unit_path.clone(),
        })
    }

    fn start(&self) -> Result<()> {
        let unit = format!("{}.service", self.unit_name);
        let out = self.systemctl(&["start", &unit])?;
        if out.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!(
            "systemctl --user start {unit} failed:\n  stderr: {stderr}\n  stdout: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }

    fn stop(&self) -> Result<()> {
        let unit = format!("{}.service", self.unit_name);
        let out = self.systemctl(&["stop", &unit])?;
        if out.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        // systemctl returns non-zero for an inactive unit on stop in
        // some versions; treat known-benign messages as success.
        if stderr.is_empty()
            || stderr.contains("not loaded")
            || stderr.contains("inactive")
            || stderr.contains("not found")
        {
            return Ok(());
        }
        anyhow::bail!(
            "systemctl --user stop {unit} failed:\n  stderr: {stderr}\n  stdout: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }

    fn uninstall(&self) -> Result<()> {
        let unit = format!("{}.service", self.unit_name);
        let _ = self.stop();
        let _ = self.systemctl(&["disable", &unit]);
        match std::fs::remove_file(&self.unit_path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(
                    anyhow::Error::new(e).context(format!("remove unit file {:?}", self.unit_path))
                );
            }
        }
        let _ = self.systemctl(&["daemon-reload"]);
        Ok(())
    }

    fn status(&self) -> Result<ServiceStatus> {
        if !self.unit_path.exists() {
            return Ok(ServiceStatus::NotInstalled);
        }
        let unit = format!("{}.service", self.unit_name);
        let out = self.systemctl(&["is-active", &unit])?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        // is-active prints "active" / "inactive" / "failed" / "activating"
        // and exits 0 only for "active".
        let line = stdout.trim();
        if line == "active" || line == "activating" {
            // Best-effort PID lookup via `show -p MainPID`.
            let pid_out = self
                .systemctl(&["show", "-p", "MainPID", "--value", &unit])
                .ok();
            let pid = pid_out
                .and_then(|o| {
                    let s = String::from_utf8_lossy(&o.stdout);
                    s.trim().parse::<u32>().ok()
                })
                .filter(|p| *p > 0);
            Ok(ServiceStatus::Running { pid })
        } else {
            Ok(ServiceStatus::Stopped)
        }
    }

    fn unit_path(&self) -> PathBuf {
        self.unit_path.clone()
    }

    fn name(&self) -> &'static str {
        "systemd-user"
    }
}

/// Render the systemd unit file for `spec`.
pub(super) fn render_unit_file(spec: &ServiceSpec) -> String {
    // Quote each argument by escaping spaces and special chars per
    // systemd's exec spec — values can be wrapped in double quotes,
    // and double quotes / backslashes inside the value escape with `\`.
    let exec_start = std::iter::once(spec.binary.to_string_lossy().into_owned())
        .chain(std::iter::once("serve".to_string()))
        .chain(spec.args.iter().cloned())
        .map(|s| systemd_quote(&s))
        .collect::<Vec<_>>()
        .join(" ");
    let install_section = if spec.at_login {
        "\n[Install]\nWantedBy=default.target\n"
    } else {
        // When the user opts out of at-login, omit the [Install]
        // section. `systemctl --user enable` would no-op.
        ""
    };
    format!(
        "[Unit]\n\
         Description=ctxd — context substrate daemon for AI agents\n\
         Documentation=https://ctxd.dev\n\
         After=network.target\n\
         \n\
         [Service]\n\
         Type=notify\n\
         ExecStart={exec_start}\n\
         WorkingDirectory={wd}\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         StandardOutput=journal\n\
         StandardError=journal\n\
         {install}",
        exec_start = exec_start,
        wd = systemd_quote(&spec.working_dir.to_string_lossy()),
        install = install_section,
    )
}

/// Quote a value for systemd's ExecStart= and other path-shaped
/// fields. Wraps in double quotes only when needed (whitespace or
/// special chars), and escapes embedded `"` / `\\` / `$`.
fn systemd_quote(s: &str) -> String {
    let needs_quotes = s
        .chars()
        .any(|c| c.is_whitespace() || matches!(c, '"' | '\\' | '$' | '#' | ';'));
    if !needs_quotes {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' | '\\' | '$' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_spec() -> ServiceSpec {
        ServiceSpec {
            binary: "/usr/local/bin/ctxd".into(),
            args: vec![
                "--db".into(),
                "/home/me/.local/share/ctxd/ctxd.db".into(),
                "--bind".into(),
                "127.0.0.1:7777".into(),
            ],
            at_login: false,
            working_dir: "/home/me/.local/share/ctxd".into(),
            log_dir: "/home/me/.local/state/ctxd/logs".into(),
            label: "dev.ctxd.daemon".into(),
        }
    }

    #[test]
    fn unit_name_strips_reverse_dns() {
        assert_eq!(Systemd::unit_name("dev.ctxd.daemon"), "ctxd");
        assert_eq!(Systemd::unit_name("dev.ctxd.service"), "ctxd");
        assert_eq!(Systemd::unit_name("ctxd"), "ctxd");
        assert_eq!(Systemd::unit_name("custom.label"), "label");
    }

    #[test]
    fn unit_file_uses_type_notify() {
        let s = render_unit_file(&fixture_spec());
        assert!(s.contains("Type=notify"));
    }

    #[test]
    fn unit_file_includes_exec_start_with_serve() {
        let s = render_unit_file(&fixture_spec());
        assert!(s.contains("ExecStart=/usr/local/bin/ctxd serve --db"));
        assert!(s.contains("--bind 127.0.0.1:7777"));
    }

    #[test]
    fn unit_file_at_login_adds_install_section() {
        let mut spec = fixture_spec();
        spec.at_login = true;
        let s = render_unit_file(&spec);
        assert!(s.contains("[Install]"));
        assert!(s.contains("WantedBy=default.target"));
    }

    #[test]
    fn unit_file_default_omits_install_section() {
        let s = render_unit_file(&fixture_spec());
        assert!(!s.contains("[Install]"));
    }

    #[test]
    fn unit_file_restart_on_failure() {
        let s = render_unit_file(&fixture_spec());
        assert!(s.contains("Restart=on-failure"));
        // Crucially, NOT `Restart=always` — we want graceful exit
        // (offboard) to actually stop the daemon.
        assert!(!s.contains("Restart=always"));
    }

    #[test]
    fn systemd_quote_passes_safe_paths_unmodified() {
        assert_eq!(systemd_quote("/usr/local/bin/ctxd"), "/usr/local/bin/ctxd");
        assert_eq!(systemd_quote("--bind"), "--bind");
        assert_eq!(systemd_quote("127.0.0.1:7777"), "127.0.0.1:7777");
    }

    #[test]
    fn systemd_quote_wraps_paths_with_spaces() {
        assert_eq!(systemd_quote("/path/with space"), "\"/path/with space\"");
    }

    #[test]
    fn systemd_quote_escapes_dollar_and_quotes() {
        assert_eq!(systemd_quote("a $b"), "\"a \\$b\"");
        assert_eq!(systemd_quote("with \"quotes\""), "\"with \\\"quotes\\\"\"");
    }

    #[test]
    fn install_creates_then_unchanged_then_updated() {
        let dir = tempfile::tempdir().unwrap();
        let unit_path = dir.path().join("ctxd.service");
        let backend = Systemd {
            unit_name: "ctxd".into(),
            unit_path: unit_path.clone(),
        };
        let mut spec = fixture_spec();
        spec.working_dir = dir.path().to_path_buf();

        let r1 = backend.install(&spec).unwrap();
        assert_eq!(r1.action, InstallAction::Created);
        assert!(unit_path.exists());

        let r2 = backend.install(&spec).unwrap();
        assert_eq!(r2.action, InstallAction::Unchanged);

        spec.binary = "/usr/local/bin/different".into();
        let r3 = backend.install(&spec).unwrap();
        assert_eq!(r3.action, InstallAction::Updated);
        let on_disk = std::fs::read_to_string(&unit_path).unwrap();
        assert!(on_disk.contains("different"));
    }

    #[test]
    fn status_reports_not_installed_when_unit_missing() {
        let dir = tempfile::tempdir().unwrap();
        let backend = Systemd {
            unit_name: "ctxd".into(),
            unit_path: dir.path().join("missing.service"),
        };
        assert_eq!(backend.status().unwrap(), ServiceStatus::NotInstalled);
    }

    #[test]
    fn uninstall_is_noop_when_not_installed() {
        let dir = tempfile::tempdir().unwrap();
        let backend = Systemd {
            unit_name: "ctxd".into(),
            unit_path: dir.path().join("missing.service"),
        };
        backend
            .uninstall()
            .expect("uninstall must be noop on clean system");
    }
}
