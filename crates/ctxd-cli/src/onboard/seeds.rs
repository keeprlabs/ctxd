//! Seed three baseline events under `/me/**` during onboard.
//!
//! When the user opens any connected AI agent and asks "what do you
//! know about me?", the answer must not be empty. Seeding populates:
//!
//! * `/me/profile` (`ctx.fact`) — hostname, platform, optional git
//!   `user.name` / `user.email` from `git config --global`,
//!   `onboarded_at` timestamp.
//! * `/me/preferences` (`ctx.note`) — placeholder + hint text.
//! * `/me/about` (`ctx.note`) — short welcome message explaining
//!   what ctxd is.
//!
//! Idempotent: a re-seed against a store that already has events at
//! these subjects skips the write — so re-running `ctxd onboard`
//! doesn't pile up duplicates. The user can opt into overwrite via
//! [`seed_force`] if they want to refresh the profile after a
//! hostname change.

use anyhow::{Context, Result};
use ctxd_core::event::Event;
use ctxd_core::subject::Subject;
use ctxd_store::EventStore;
use std::path::Path;

/// Outcome of a seed pass.
#[derive(Debug, Clone, Default)]
pub struct SeedReport {
    /// Subjects that received fresh events.
    pub created: Vec<String>,
    /// Subjects that already had at least one event and were skipped.
    pub skipped: Vec<String>,
}

/// Seed `/me/profile`, `/me/preferences`, `/me/about` if absent.
/// Returns a report summarising what was written and what was
/// skipped.
pub async fn seed(store: &EventStore) -> Result<SeedReport> {
    let mut report = SeedReport::default();
    seed_one(
        store,
        "/me/profile",
        "ctx.fact",
        profile_payload(),
        &mut report,
    )
    .await?;
    seed_one(
        store,
        "/me/preferences",
        "ctx.note",
        preferences_payload(),
        &mut report,
    )
    .await?;
    seed_one(store, "/me/about", "ctx.note", about_payload(), &mut report).await?;
    Ok(report)
}

/// Same as [`seed`] but writes regardless of pre-existing events.
/// The new events become the latest under each subject; older
/// events stay in the log for audit / read-at queries.
pub async fn seed_force(store: &EventStore) -> Result<SeedReport> {
    let mut report = SeedReport::default();
    write_event(store, "/me/profile", "ctx.fact", profile_payload()).await?;
    report.created.push("/me/profile".to_string());
    write_event(store, "/me/preferences", "ctx.note", preferences_payload()).await?;
    report.created.push("/me/preferences".to_string());
    write_event(store, "/me/about", "ctx.note", about_payload()).await?;
    report.created.push("/me/about".to_string());
    Ok(report)
}

/// Convenience wrapper: open the store at `db_path`, seed, return.
pub async fn seed_at_path(db_path: &Path) -> Result<SeedReport> {
    let store = EventStore::open(db_path)
        .await
        .with_context(|| format!("open store at {db_path:?}"))?;
    seed(&store).await
}

async fn seed_one(
    store: &EventStore,
    subject: &str,
    event_type: &str,
    payload: serde_json::Value,
    report: &mut SeedReport,
) -> Result<()> {
    let s = Subject::new(subject).with_context(|| format!("invalid subject {subject}"))?;
    let existing = store
        .read(&s, false)
        .await
        .with_context(|| format!("read {subject}"))?;
    if !existing.is_empty() {
        report.skipped.push(subject.to_string());
        return Ok(());
    }
    write_event(store, subject, event_type, payload).await?;
    report.created.push(subject.to_string());
    Ok(())
}

async fn write_event(
    store: &EventStore,
    subject: &str,
    event_type: &str,
    payload: serde_json::Value,
) -> Result<()> {
    let s = Subject::new(subject).with_context(|| format!("invalid subject {subject}"))?;
    let event = Event::new(
        "ctxd://onboard".to_string(),
        s,
        event_type.to_string(),
        payload,
    );
    store
        .append(event)
        .await
        .with_context(|| format!("append to {subject}"))?;
    Ok(())
}

fn profile_payload() -> serde_json::Value {
    let hostname = hostname().unwrap_or_else(|| "unknown".to_string());
    let platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    let onboarded_at = chrono::Utc::now().to_rfc3339();
    let mut obj = serde_json::Map::new();
    obj.insert("hostname".into(), serde_json::Value::String(hostname));
    obj.insert("platform".into(), serde_json::Value::String(platform));
    obj.insert(
        "onboarded_at".into(),
        serde_json::Value::String(onboarded_at),
    );
    if let Some(name) = git_config("user.name") {
        obj.insert("git_user_name".into(), serde_json::Value::String(name));
    }
    if let Some(email) = git_config("user.email") {
        obj.insert("git_user_email".into(), serde_json::Value::String(email));
    }
    serde_json::Value::Object(obj)
}

fn preferences_payload() -> serde_json::Value {
    serde_json::json!({
        "text": "No preferences captured yet. Tell your AI things like \
                 'I prefer TypeScript for new projects' or \
                 'remember I'm a vegetarian' — they'll be saved here.",
        "hint": "preferences-empty",
    })
}

fn about_payload() -> serde_json::Value {
    serde_json::json!({
        "text": "ctxd is your shared memory across AI tools. Anything you tell \
                 Claude Desktop, Claude Code, or Codex lands in the same context \
                 graph — so a fact you taught one assistant is visible to all of \
                 them. Run `ctxd doctor` anytime to see status.",
        "hint": "welcome",
    })
}

/// Best-effort hostname lookup. Returns `None` if the platform
/// doesn't expose one cheaply.
fn hostname() -> Option<String> {
    if let Ok(h) = std::env::var("HOSTNAME") {
        if !h.is_empty() {
            return Some(h);
        }
    }
    // On macOS / Linux, `gethostname(3)` via the libc crate is
    // reliable. Fall back to `hostname` command if that fails.
    #[cfg(unix)]
    {
        let mut buf = [0u8; 256];
        // SAFETY: gethostname writes up to len bytes and we read
        // only up to the first NUL.
        let rc = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut _, buf.len()) };
        if rc == 0 {
            let nul = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
            return Some(String::from_utf8_lossy(&buf[..nul]).into_owned());
        }
    }
    None
}

/// Read a `git config --global <key>` value. Returns `None` if git
/// is not installed or the key is unset.
fn git_config(key: &str) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["config", "--global", key])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn seed_writes_three_subjects_on_empty_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = EventStore::open(&dir.path().join("ctxd.db")).await.unwrap();
        let report = seed(&store).await.unwrap();
        assert_eq!(
            report.created.len(),
            3,
            "expected 3 created, got {report:?}"
        );
        assert!(report.skipped.is_empty());
        // Each subject must now have exactly one event.
        for subj in ["/me/profile", "/me/preferences", "/me/about"] {
            let s = Subject::new(subj).unwrap();
            let events = store.read(&s, false).await.unwrap();
            assert_eq!(events.len(), 1, "{subj} should have 1 event");
        }
    }

    #[tokio::test]
    async fn seed_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store = EventStore::open(&dir.path().join("ctxd.db")).await.unwrap();
        seed(&store).await.unwrap();
        let r2 = seed(&store).await.unwrap();
        assert!(r2.created.is_empty(), "second seed must not create: {r2:?}");
        assert_eq!(r2.skipped.len(), 3);
        // Confirm the log didn't grow on the second seed.
        for subj in ["/me/profile", "/me/preferences", "/me/about"] {
            let s = Subject::new(subj).unwrap();
            let events = store.read(&s, false).await.unwrap();
            assert_eq!(events.len(), 1, "{subj} must have 1 event after re-seed");
        }
    }

    #[tokio::test]
    async fn seed_force_writes_a_second_event() {
        let dir = tempfile::tempdir().unwrap();
        let store = EventStore::open(&dir.path().join("ctxd.db")).await.unwrap();
        seed(&store).await.unwrap();
        seed_force(&store).await.unwrap();
        let s = Subject::new("/me/profile").unwrap();
        let events = store.read(&s, false).await.unwrap();
        assert_eq!(
            events.len(),
            2,
            "force seed must add a second profile event"
        );
    }

    #[test]
    fn profile_payload_has_required_keys() {
        let v = profile_payload();
        assert!(v.get("hostname").is_some());
        assert!(v.get("platform").is_some());
        assert!(v.get("onboarded_at").is_some());
        assert!(v["platform"]
            .as_str()
            .unwrap()
            .contains(std::env::consts::OS));
    }

    #[test]
    fn preferences_payload_has_hint_and_text() {
        let v = preferences_payload();
        assert_eq!(v["hint"], "preferences-empty");
        assert!(v["text"].as_str().unwrap().contains("AI"));
    }

    #[test]
    fn about_payload_has_welcome_hint() {
        let v = about_payload();
        assert_eq!(v["hint"], "welcome");
    }

    #[test]
    fn hostname_returns_something_on_unix() {
        #[cfg(unix)]
        assert!(hostname().is_some(), "hostname() should resolve on unix");
    }
}
