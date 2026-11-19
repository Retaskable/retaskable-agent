//! A GitLab project (`codeHost.kind` `gitlab`): the TS agent's host has no `commitChecks`, so the pass and the Fix
//! button do nothing there. The fake `gh` records every call; none is made. GitHub's behaviour is the other tests'.

use super::*;
use crate::ci_pass::press::PressError;
use crate::config::{CodeHostConfig, ProjectConfig};

const NOT_THE_ONE: &str = "This computer isn't the one watching this project's pull requests.";

async fn gitlab_rig() -> Rig {
    let mut rig = Rig::new().await;
    rig.gitlab = true;
    // A red pull request that a GitHub project would have judged and fixed.
    list(rig.dir.path(), vec![pull(9, "feat/a", "sha9")]);
    rig
}

#[tokio::test]
async fn the_pass_does_nothing_and_never_runs_gh() {
    let rig = gitlab_rig().await;
    let mut since = HashMap::new();
    assert_eq!(rig.pass(&mut since, 1_000).await.unwrap(), 0);
    assert!(rig.gh_log().is_empty(), "{:?}", rig.gh_log());
    assert!(rig.store.sent("ci_result").is_empty());
    assert!(since.is_empty());
}

#[tokio::test]
async fn the_fix_button_refuses_in_the_words_of_not_being_the_watcher() {
    let rig = gitlab_rig().await;
    let judge = Arc::new(FakeJudge {
        asked: Mutex::default(),
        says: None,
    });
    let fixer = Arc::new(FakeFixer {
        asked: Mutex::default(),
        acts: Mutex::new(Box::new(|_| true)),
        contexts: Mutex::default(),
        says: Mutex::default(),
    });
    let press = Arc::new(crate::ci_pass::press::Press {
        host: rig.host(),
        judge,
        fixer,
        client: Arc::new(Client::new(&rig.store.url)),
        repo: 7,
        token: "tok".into(),
        items: HashSet::new(),
        allow: Vec::new(),
        logins: Arc::new(LoginCache::default()),
        bell: Arc::new(|_, _| {}),
        fixing: rig.fixing.clone(),
        runner: Arc::new(std::sync::atomic::AtomicBool::new(true)),
    });
    assert_eq!(
        press.begin(9, "sha9").await.err(),
        Some(PressError::Refused(NOT_THE_ONE.into()))
    );
    assert!(rig.gh_log().is_empty(), "{:?}", rig.gh_log());
    assert!(!rig.fixing.holds(9, "sha9"));
}

#[test]
fn only_a_config_without_codehost_or_with_github_is_on_github() {
    let on = |kind: Option<&str>| {
        ProjectConfig {
            code_host: kind.map(|k| CodeHostConfig {
                kind: k.into(),
                project: None,
            }),
            ..Default::default()
        }
        .on_github()
    };
    assert!(on(None));
    assert!(on(Some("github")));
    assert!(!on(Some("gitlab")));
    let read: ProjectConfig =
        serde_json::from_value(json!({ "codeHost": { "kind": "gitlab", "project": "g/p" } })).unwrap();
    assert!(!read.on_github());
}
