//! Unlink this project (`driver/unlink.rs`): refused while anything runs, otherwise answered first, the store's `leave` run, and
//! the project taken out of the config with its marker and lanes.
use super::super::asks::{Listed, ops_named, store_failing};
use super::*;
use crate::view::{ProjectCommand, Reply};

struct Unlinking {
    rig: Rig,
    asked: Asked,
    data: tempfile::TempDir,
    answers: tokio::sync::mpsc::UnboundedReceiver<Value>,
    done: tokio::sync::oneshot::Receiver<Result<(), String>>,
}

/// A loop in `claim` mode whose config names projects "p0" and "p1", with an unlink of "p0" queued.
async fn unlinking(remove_lanes: bool) -> (Unlinking, Listed) {
    let (base, asked, listed) = store_failing("").await;
    let mut rig = rig(2, &[&base]);
    rig.go_live();
    rig.machine.wanted = true;
    rig.lp(0).wanted.poll_every = Duration::ZERO;
    let data = tempfile::tempdir().unwrap();
    let worktrees = data.path().join("worktrees-p0");
    std::fs::create_dir_all(worktrees.join("lane-1")).unwrap();
    std::fs::write(
        data.path().join("config.json"),
        json!({ "version": 2, "projects": [
            { "id": "p0", "repoPath": data.path().join("nowhere"), "worktreesDir": worktrees },
            { "id": "p1" }
        ] })
        .to_string(),
    )
    .unwrap();
    crate::start_paused::hold(data.path(), "p0").unwrap();
    crate::start_paused::hold(data.path(), "p1").unwrap();
    rig.lp(0).data_dir = Some(data.path().to_path_buf());
    let lp = rig.lp(0);
    for project in [&mut lp.project, &mut lp.file_project] {
        project.worktrees_dir = worktrees.to_string_lossy().into_owned();
        project.repo_path = data.path().join("nowhere").to_string_lossy().into_owned();
    }
    let items = rig.machine.links.item_commands.clone();
    let (tx, answers) = tokio::sync::mpsc::unbounded_channel();
    items.set_out("p0", Some(tx));
    let (sender, done) = tokio::sync::oneshot::channel();
    items.projects.push(ProjectCommand::Unlink {
        project: "p0".into(),
        command_id: "c1".into(),
        remove_lanes,
        done: Reply::new(sender),
    });
    (
        Unlinking {
            rig,
            asked,
            data,
            answers,
            done,
        },
        listed,
    )
}

fn config_ids(dir: &std::path::Path) -> Vec<String> {
    let cfg: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("config.json")).unwrap()).unwrap();
    (cfg["projects"].as_array().unwrap().iter())
        .map(|p| p["id"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn an_idle_project_is_acked_first_gives_back_its_items_and_leaves_the_config_with_its_lanes() {
    let (mut u, _) = unlinking(true).await;
    u.rig.machine.pass().await;
    assert_eq!(u.answers.try_recv().unwrap(), json!({ "type": "ack", "id": "c1" }));
    assert_eq!(u.done.try_recv().unwrap(), Ok(()));
    let left = ops_named(&u.asked, "leave");
    assert_eq!(left.len(), 1);
    assert_eq!(left[0]["why"], format!("unlinked on {}", u.rig.machine.links.machine));
    assert_eq!(config_ids(u.data.path()), ["p1"]);
    let marker = std::fs::read_to_string(u.data.path().join("start-paused.json")).unwrap();
    assert!(!marker.contains("p0") && marker.contains("p1"), "{marker}");
    assert!(!u.data.path().join("worktrees-p0").exists(), "lanes removed");
    // No new work starts in a project on its way out.
    assert_eq!(u.rig.lp(0).mode, OrchestratorMode::Off);
}

#[tokio::test]
async fn with_remove_lanes_false_the_lanes_stay() {
    let (mut u, _) = unlinking(false).await;
    u.rig.machine.pass().await;
    assert_eq!(u.done.try_recv().unwrap(), Ok(()));
    assert_eq!(config_ids(u.data.path()), ["p1"]);
    assert!(u.data.path().join("worktrees-p0/lane-1").exists());
}

#[tokio::test]
async fn anything_running_refuses_it_in_ts_words_and_changes_nothing() {
    let (mut u, _) = unlinking(true).await;
    u.rig.queue(0, &["a"]);
    u.rig.lp(0).runs.get_mut("a").unwrap().state = ItemState::Implementing;
    u.rig.machine.pass().await;
    let nack = u.answers.try_recv().unwrap();
    assert_eq!((nack["type"].as_str(), nack["id"].as_str()), (Some("nack"), Some("c1")));
    let reason = nack["reason"].as_str().unwrap();
    assert!(
        reason.starts_with("Sessions are running in ") && reason.ends_with(": pause it and let it finish first."),
        "{reason}"
    );
    assert_eq!(u.done.try_recv().unwrap(), Err(reason.to_string()));
    assert!(ops_named(&u.asked, "leave").is_empty());
    assert_eq!(config_ids(u.data.path()), ["p0", "p1"]);
    assert!(u.data.path().join("worktrees-p0/lane-1").exists());
    assert_eq!(u.rig.lp(0).mode, OrchestratorMode::Claim);
}

#[tokio::test]
async fn a_held_lane_for_play_refuses_it_too() {
    let (mut u, _) = unlinking(true).await;
    assert!(u.rig.machine.pool.take(
        1,
        crate::lane_pool::Holder::Play {
            project: "p0".into(),
            item: "a".into()
        }
    ));
    u.rig.machine.pass().await;
    let nack = u.answers.try_recv().unwrap();
    assert!(
        nack["reason"].as_str().unwrap().starts_with("Play is running in "),
        "{nack}"
    );
}

#[tokio::test]
async fn a_store_that_cannot_be_asked_does_not_stop_the_unlink() {
    let (mut u, listed) = unlinking(true).await;
    listed.refused.lock().unwrap().insert("leave".into(), (500, "boom"));
    u.rig.machine.pass().await;
    assert_eq!(u.done.try_recv().unwrap(), Ok(()));
    assert_eq!(config_ids(u.data.path()), ["p1"]);
}

#[tokio::test]
async fn a_config_that_cannot_be_written_puts_the_project_back_as_it_was() {
    let (mut u, _) = unlinking(true).await;
    std::fs::write(
        u.data.path().join("config.json"),
        json!({ "version": 2, "projects": [{ "id": "other" }] }).to_string(),
    )
    .unwrap();
    u.rig.machine.pass().await;
    assert_eq!(u.answers.try_recv().unwrap()["type"], "ack");
    let why = u.done.try_recv().unwrap().unwrap_err();
    assert!(why.contains("p0 isn't in config.json"), "{why}");
    assert_eq!(u.rig.lp(0).mode, OrchestratorMode::Claim);
}
