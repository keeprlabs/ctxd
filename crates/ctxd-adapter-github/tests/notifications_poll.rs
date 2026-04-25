//! /notifications returns three items → three notification.received events.

mod common;

use ctxd_adapter_github::config::ResourceKind;
use ctxd_adapter_github::GitHubAdapter;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn notif(id: &str, reason: &str) -> serde_json::Value {
    json!({
        "id": id,
        "unread": true,
        "reason": reason,
        "updated_at": "2026-04-10T00:00:00Z",
        "subject": { "title": "PR title", "type": "PullRequest", "url": "https://api.github.com/repos/acme/web/pulls/7" },
        "repository": { "full_name": "acme/web" },
    })
}

#[tokio::test]
async fn notifications_emits_three_events() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();

    Mock::given(method("GET"))
        .and(path("/notifications"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            notif("1", "subscribed"),
            notif("2", "mention"),
            notif("3", "review_requested"),
        ])))
        .mount(&server)
        .await;

    // No repos configured — selecting only Notifications.
    let cfg = common::explicit_config(
        &server.uri(),
        dir.path().to_path_buf(),
        vec![],
        vec![ResourceKind::Notifications],
        true,
    );

    let adapter = GitHubAdapter::new(cfg);
    let (sink, events) = common::CollectingSink::new();
    adapter.run_once(&sink).await.unwrap();

    let evs = events.lock().await.clone();
    assert_eq!(evs.len(), 3, "got {evs:?}");
    for e in &evs {
        assert_eq!(e.event_type, "notification.received");
        assert!(e.subject.starts_with("/work/github/notifications/"));
    }
    let ids: Vec<String> = evs
        .iter()
        .map(|e| e.data["id"].as_str().unwrap().to_string())
        .collect();
    assert!(ids.contains(&"1".to_string()));
    assert!(ids.contains(&"2".to_string()));
    assert!(ids.contains(&"3".to_string()));
}
