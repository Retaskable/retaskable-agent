//! The commands of the second command sweep that are checked and queued here (`answer`, `unlink_project`, the reads and the
//! item and lane commands the hub still pushes).
use super::*;

fn go(v: &AgentView, name: &str, args: Value) -> Value {
    run(&cmd(name, args), "one", &mut Seen::default(), v, &|_| {}, &data_dir())
}

#[test]
fn answer_queues_the_answers_in_order_and_needs_the_item_its_answers_and_claim_mode() {
    let v = claiming();
    assert_eq!(
        go(&v, "answer", json!({ "itemId": " a ", "answers": ["Blue.", ""] })),
        json!({ "type": "ack", "id": "c1" })
    );
    let ItemCommand::Answer {
        item,
        answers,
        attached,
        ..
    } = v.item_commands.take("one").remove(0)
    else {
        panic!("not an answer")
    };
    assert_eq!(
        (item.as_str(), answers),
        ("a", vec!["Blue.".to_string(), String::new()])
    );
    assert!(attached.is_none());
    for args in [
        json!({ "answers": ["x"] }),
        json!({ "itemId": "a" }),
        json!({ "itemId": "a", "answers": [1] }),
    ] {
        assert_eq!(go(&v, "answer", args)["type"], "nack");
    }
    assert!(v.item_commands.take("one").is_empty());
    let off = view();
    let nack = go(&off, "answer", json!({ "itemId": "a", "answers": ["x"] }));
    assert_eq!(
        nack["reason"],
        "This computer isn't running work (the orchestrator isn't in claim mode)."
    );
}

#[test]
fn answer_with_pictures_goes_up_pending_for_the_loop_to_fetch_them() {
    let v = claiming();
    let args = json!({ "itemId": "a", "answers": ["See"], "pictureIds": [picture_id('b')] });
    assert_eq!(go(&v, "answer", args), json!({ "type": "pending", "id": "c1" }));
    let ItemCommand::Answer { attached, .. } = v.item_commands.take("one").remove(0) else {
        panic!("not an answer")
    };
    assert_eq!(attached.unwrap().ids, vec![picture_id('b')]);
}

mod unlink {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::view::ProjectCommand;

    /// The restart is counted and announced.
    #[derive(Clone, Default)]
    struct Restarts(Arc<AtomicUsize>, Arc<tokio::sync::Notify>);

    fn adder(dir: &std::path::Path, restarts: Restarts) -> Arc<crate::add_project::AddProject> {
        crate::add_project::AddProject::with(
            dir.to_path_buf(),
            crate::env::Env::empty(),
            crate::repo_picker::RepoPicker::with(
                Box::new(|| Ok(vec![])),
                Box::new(|_| Ok(vec![])),
                Box::new(std::time::Instant::now),
            ),
            Box::new(|_, _| Ok(())),
            Box::new(|_| None),
            Box::new(|job| {
                std::thread::spawn(job);
            }),
            Box::new(move || {
                restarts.0.fetch_add(1, Ordering::SeqCst);
                restarts.1.notify_one();
            }),
        )
    }

    fn unlinking(dir: &std::path::Path, restarts: Restarts) -> AgentView {
        let mut v = claiming();
        v.settings = crate::hello_settings::HelloSettings::new(dir.to_path_buf(), crate::env::Env::empty(), 4700);
        v.add_project.set(adder(dir, restarts));
        v
    }

    #[tokio::test]
    async fn it_is_queued_for_the_loop_and_pending_and_the_state_folder_goes_before_the_restart() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::paths::project_dir(dir.path(), "one");
        std::fs::create_dir_all(&state).unwrap();
        let restarts = Restarts::default();
        let v = unlinking(dir.path(), restarts.clone());
        let restarted = restarts.1.notified();
        tokio::pin!(restarted);
        assert_eq!(
            go(&v, "unlink_project", json!({})),
            json!({ "type": "pending", "id": "c1" })
        );
        assert_eq!(
            go(&v, "unlink_project", json!({ "removeLanes": false }))["type"],
            "pending"
        );
        let queued = v.item_commands.projects.take("one");
        let flags: Vec<bool> = queued
            .iter()
            .filter_map(|c| match c {
                ProjectCommand::Unlink { remove_lanes, .. } => Some(*remove_lanes),
                _ => None,
            })
            .collect();
        assert_eq!(flags, [true, false]);
        // Nothing is removed and nothing restarts until the loop says it is over.
        tokio::task::yield_now().await;
        assert!(state.exists() && restarts.0.load(Ordering::SeqCst) == 0);
        let ProjectCommand::Unlink { done, .. } = &queued[0] else {
            panic!("not an unlink")
        };
        done.send(Ok(()));
        tokio::time::timeout(std::time::Duration::from_secs(30), restarted)
            .await
            .unwrap();
        assert!(!state.exists());
        assert_eq!(restarts.0.load(Ordering::SeqCst), 1);
        // A refusal restarts nothing and keeps the folder.
        let ProjectCommand::Unlink { done, .. } = &queued[1] else {
            panic!("not an unlink")
        };
        done.send(Err("A step is running".into()));
        tokio::task::yield_now().await;
        assert_eq!(restarts.0.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn it_needs_the_agents_own_unlinking_the_settings_folder_and_a_loop_that_acts() {
        let plain = claiming();
        assert_eq!(
            go(&plain, "unlink_project", json!({}))["reason"],
            "This agent can't unlink a project from retaskable.com."
        );
        let dir = tempfile::tempdir().unwrap();
        let mut no_dir = unlinking(dir.path(), Default::default());
        no_dir.settings = Default::default();
        assert_eq!(
            go(&no_dir, "unlink_project", json!({}))["reason"],
            "This computer's settings can't be found."
        );
        let off = unlinking(dir.path(), Default::default());
        off.item_commands.set_claim(false);
        assert_eq!(go(&off, "unlink_project", json!({}))["type"], "nack");
        assert!(off.item_commands.projects.take("one").is_empty());
    }
}

mod release_page {
    use std::sync::Arc;

    use futures_util::FutureExt;

    use super::*;
    use crate::link::TokenAnswer;
    use crate::view::{CloudToken, ProjectCommand};

    struct Cloud;

    impl CloudToken for Cloud {
        fn url(&self) -> &str {
            "https://retaskable.com"
        }

        fn answer(&self) -> futures_util::future::BoxFuture<'_, TokenAnswer> {
            async { TokenAnswer::Idle }.boxed()
        }
    }

    #[test]
    fn publish_and_unpublish_are_queued_with_the_agents_own_cloud_and_refused_without_one_or_a_loop() {
        let v = claiming();
        for name in ["release_publish", "release_unpublish"] {
            assert_eq!(
                go(&v, name, json!({}))["reason"],
                "Release notes are published on this agent's own retaskable.com, not this one: ask from there."
            );
        }
        v.item_commands.cloud.set_primary(Arc::new(Cloud));
        assert_eq!(
            go(&v, "release_publish", json!({})),
            json!({ "type": "pending", "id": "c1" })
        );
        assert_eq!(go(&v, "release_unpublish", json!({}))["type"], "pending");
        let queued = v.item_commands.projects.take("one");
        let kinds: Vec<bool> = queued
            .iter()
            .map(|c| matches!(c, ProjectCommand::Release { unpublish: true, .. }))
            .collect();
        assert_eq!(kinds, [false, true]);
        let off = view();
        off.item_commands.cloud.set_primary(Arc::new(Cloud));
        let nack = go(&off, "release_publish", json!({}));
        assert_eq!(
            nack["reason"],
            "This computer isn't running work (the orchestrator isn't in claim or runs mode)."
        );
    }
}
