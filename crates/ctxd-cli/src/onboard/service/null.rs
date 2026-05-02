//! Stub [`super::ServiceBackend`] for platforms we don't yet target
//! (Windows in v0.4, BSDs ever).
//!
//! Every method returns a clear "not supported" error so callers get
//! one consistent failure mode rather than silent no-ops. The
//! [`super::is_supported`] gate exists so callers can branch
//! without triggering errors.

use super::{InstallReport, ServiceBackend, ServiceSpec, ServiceStatus};
use anyhow::Result;
use std::path::PathBuf;

/// No-op backend. Every operation errors with a descriptive message.
pub struct NullBackend {
    label: String,
}

impl NullBackend {
    pub fn new(label: String) -> Self {
        Self { label }
    }

    fn unsupported<T>(&self) -> Result<T> {
        anyhow::bail!(
            "service install for label `{}` not supported on this platform yet \
             (v0.4 ships macOS + Linux; Windows is planned for v0.5). Run \
             `ctxd serve` in a foreground terminal as a workaround.",
            self.label
        )
    }
}

impl ServiceBackend for NullBackend {
    fn install(&self, _spec: &ServiceSpec) -> Result<InstallReport> {
        self.unsupported()
    }

    fn start(&self) -> Result<()> {
        self.unsupported()
    }

    fn stop(&self) -> Result<()> {
        self.unsupported()
    }

    fn uninstall(&self) -> Result<()> {
        self.unsupported()
    }

    fn status(&self) -> Result<ServiceStatus> {
        // Status is the one thing we can answer truthfully without
        // a backend: NotInstalled.
        Ok(ServiceStatus::NotInstalled)
    }

    fn unit_path(&self) -> PathBuf {
        PathBuf::new()
    }

    fn name(&self) -> &'static str {
        "null"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_backend_status_is_not_installed() {
        let b = NullBackend::new("dev.ctxd.daemon".into());
        assert_eq!(b.status().unwrap(), ServiceStatus::NotInstalled);
    }

    #[test]
    fn null_backend_install_errors_clearly() {
        let b = NullBackend::new("dev.ctxd.daemon".into());
        let spec = ServiceSpec {
            binary: "/x".into(),
            args: vec![],
            at_login: false,
            working_dir: "/".into(),
            log_dir: "/".into(),
            label: "dev.ctxd.daemon".into(),
        };
        let err = b.install(&spec).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("not supported"));
        assert!(msg.contains("Windows"));
    }
}
