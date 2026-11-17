//! The snapshot sends no `smoke`: the staging smoke is the store's `smoke` fact (the hub drops the key, `HUB_DROPS`).
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
async fn the_smoke_is_never_in_the_snapshot_it_is_the_stores_fact() {
    for status in [SmokeStatus::Passed, SmokeStatus::Failed, SmokeStatus::Timeout] {
        let sent = first_snapshot("smoke-held", Some(state(status, Some("boom")))).await;
        assert!(sent["state"].get("smoke").is_none(), "{sent}");
    }
}
