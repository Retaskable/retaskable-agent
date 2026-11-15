//! The snapshot's `smoke`: the store's staging smoke fact, as the loop read it.
use super::*;
use crate::store::{SmokeState, SmokeStatus};

fn on(view: &mut Arc<AgentView>, dir: &std::path::Path) {
    Arc::get_mut(view).unwrap().settings =
        crate::hello_settings::HelloSettings::new(dir.to_path_buf(), crate::env::Env::empty(), 4700);
}

fn state(status: SmokeStatus, tail: Option<&str>) -> SmokeState {
    SmokeState {
        sha: "abc123".into(),
        status,
        at: "2026-11-08T00:00:00.000Z".into(),
        url: Some("https://staging.example.com".into()),
        tail: tail.map(String::from),
    }
}

fn save(view: &AgentView, project: &str, smoke: &SmokeState) {
    view.live.register(project);
    view.live.set_shown(
        project,
        crate::backlog::shown::Shown {
            smoke: Some(smoke.clone()),
            vet: None,
        },
    );
}

async fn first_snapshot(project: &str, smoke: Option<SmokeState>) -> Value {
    let (url, _, mut rx) = cloud().await;
    let (dir, store) = linked(&url);
    let mut view = view(&[project]);
    on(&mut view, dir.path());
    if let Some(s) = &smoke {
        save(&view, project, s);
    }
    let c = start_connector(&url, store, &view, dir.path());
    let mut first = next(&mut rx).await;
    read(&mut first).await; // hello
    let snapshot = read(&mut first).await;
    c.stop(Duration::from_secs(1)).await;
    snapshot
}

#[tokio::test]
async fn a_passed_smoke_is_sent_without_a_tail_and_a_failed_one_with_it() {
    let passed = first_snapshot("smoke-passed", Some(state(SmokeStatus::Passed, Some("old")))).await;
    assert_eq!(
        passed["state"]["smoke"],
        json!({ "sha": "abc123", "status": "passed", "at": "2026-11-08T00:00:00.000Z", "url": "https://staging.example.com" })
    );
    let failed = first_snapshot("smoke-failed", Some(state(SmokeStatus::Failed, Some("boom")))).await;
    assert_eq!(failed["state"]["smoke"]["status"], "failed");
    assert_eq!(failed["state"]["smoke"]["tail"], "boom");
}

#[tokio::test]
async fn no_smoke_state_sends_no_smoke() {
    let none = first_snapshot("smoke-none", None).await;
    assert!(none["state"].get("smoke").is_none());
}

#[tokio::test]
async fn a_new_smoke_fact_resends_the_snapshot() {
    let project = "smoke-resend";
    let (url, _, mut rx) = cloud().await;
    let (dir, store) = linked(&url);
    let mut view = view(&[project]);
    on(&mut view, dir.path());
    let c = start_connector(&url, store, &view, dir.path());
    let mut first = next(&mut rx).await;
    read(&mut first).await; // hello
    read(&mut first).await; // snapshot
    save(&view, project, &state(SmokeStatus::Running, None));
    let again = read_not_alive(&mut first).await;
    assert_eq!(again["type"], "snapshot");
    assert_eq!(again["state"]["smoke"]["status"], "running");
    c.stop(Duration::from_secs(1)).await;
}
