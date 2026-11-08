//! The snapshot's `smoke`: the project's staging smoke line.
use super::*;
use crate::store::{SmokeState, SmokeStatus, Store};

fn on(view: &mut Arc<AgentView>, dir: &std::path::Path) {
    Arc::get_mut(view).unwrap().settings = crate::hello_settings::HelloSettings::new(
        dir.to_path_buf(),
        crate::env::Env::empty(),
        4700,
        Arc::new(crate::flags::Flags::fixed(&["snapshot-reporting"])),
    );
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

fn save(dir: &std::path::Path, project: &str, smoke: &SmokeState) {
    let mut store = Store::load(&crate::paths::project_dir(dir, project)).unwrap();
    store.set_smoke(smoke);
    store.save().unwrap();
}

async fn first_snapshot(project: &str, flag: bool, smoke: Option<SmokeState>) -> Value {
    let (url, _, mut rx) = cloud().await;
    let (dir, store) = linked(&url);
    let mut view = view(&[project]);
    if flag {
        on(&mut view, dir.path());
    }
    if let Some(s) = &smoke {
        save(dir.path(), project, s);
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
    let passed = first_snapshot("smoke-passed", true, Some(state(SmokeStatus::Passed, Some("old")))).await;
    assert_eq!(
        passed["state"]["smoke"],
        json!({ "sha": "abc123", "status": "passed", "at": "2026-11-08T00:00:00.000Z", "url": "https://staging.example.com" })
    );
    let failed = first_snapshot("smoke-failed", true, Some(state(SmokeStatus::Failed, Some("boom")))).await;
    assert_eq!(failed["state"]["smoke"]["status"], "failed");
    assert_eq!(failed["state"]["smoke"]["tail"], "boom");
}

#[tokio::test]
async fn no_smoke_or_the_flag_off_sends_no_smoke() {
    let none = first_snapshot("smoke-none", true, None).await;
    assert!(none["state"].get("smoke").is_none());
    let off = first_snapshot("smoke-off", false, Some(state(SmokeStatus::Passed, None))).await;
    assert!(off["state"].get("smoke").is_none());
}

#[tokio::test]
async fn saving_a_new_smoke_state_resends_the_snapshot() {
    let project = "smoke-resend";
    let (url, _, mut rx) = cloud().await;
    let (dir, store) = linked(&url);
    let mut view = view(&[project]);
    on(&mut view, dir.path());
    let c = start_connector(&url, store, &view, dir.path());
    let mut first = next(&mut rx).await;
    read(&mut first).await; // hello
    read(&mut first).await; // snapshot
    save(dir.path(), project, &state(SmokeStatus::Running, None));
    let again = read_not_alive(&mut first).await;
    assert_eq!(again["type"], "snapshot");
    assert_eq!(again["state"]["smoke"]["status"], "running");
    c.stop(Duration::from_secs(1)).await;
}
