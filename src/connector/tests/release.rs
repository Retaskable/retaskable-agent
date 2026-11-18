//! Allowed to release in hello (release-allowed-switch.md section 3.1): `policy.releases` and `computer.releases`, sent
//! again when the hub's `owner_role` or this computer's `gh` login's answer changes, and only if the hello differs.
use super::*;

use std::time::Instant;

/// A computer with one project, `one`, that has a rollout whose program is installed.
fn computer_with_rollout() -> (tempfile::TempDir, Arc<AgentView>) {
    let dir = tempfile::tempdir().unwrap();
    let tool = if cfg!(windows) { "tool.exe" } else { "tool" };
    std::fs::write(dir.path().join(tool), "").unwrap();
    std::fs::write(
        dir.path().join("config.json"),
        json!({ "version": 2, "projects": [{ "id": "one", "githubRepo": "octo/game", "repoPath": "r",
            "deploy": { "staging": { "branch": "staging" },
                "production": { "branch": "main", "url": "https://x.test",
                    "rollout": { "traffic": "tool traffic", "health": "tool health" } } } }] })
        .to_string(),
    )
    .unwrap();
    let env = crate::env::Env::from_pairs([("PATH", dir.path().to_str().unwrap())]);
    let mut v = crate::test_support::agent_view(
        vec!["windows".into()],
        vec![crate::test_support::project_view("one")],
        "9.9.9",
        2,
    );
    v.settings = crate::hello_settings::HelloSettings::new(dir.path().to_path_buf(), env);
    (dir, Arc::new(v))
}

fn releases(hello: &Value) -> (Value, Value) {
    (
        hello["policy"]["releases"].clone(),
        hello["computer"]["releases"].clone(),
    )
}

#[tokio::test]
async fn hello_says_whether_it_may_release_and_again_when_the_role_or_the_login_changes() {
    let (conf, v) = computer_with_rollout();
    let (url, _, mut rx) = cloud().await;
    let (dir, store) = linked(&url);
    let c = start_connector(&url, store, &v, dir.path());
    let mut a = next(&mut rx).await;
    // Before the hub says the owner's role: the switch (default) and the project's rollout.
    let hello = read(&mut a).await;
    assert_eq!(releases(&hello), (json!(true), json!({ "on": true })));
    assert_eq!(read(&mut a).await["type"], "snapshot");

    said(&mut a, json!({ "type": "owner_role", "role": "member" })).await;
    let hello = read_not_alive(&mut a).await;
    assert_eq!(hello["type"], "hello");
    assert_eq!(
        releases(&hello),
        (
            json!(false),
            json!({ "on": false, "why": "Off: you're not an admin of octo/game" })
        )
    );

    said(&mut a, json!({ "type": "owner_role", "role": "admin" })).await;
    let hello = read_not_alive(&mut a).await;
    assert_eq!(
        hello["computer"]["releases"]["why"],
        "Off: checking this computer's GitHub login for octo/game"
    );

    // The login's answer arrives (the periodic check's job).
    v.settings.gate().record("one", true, Instant::now());
    let hello = read_not_alive(&mut a).await;
    assert_eq!(releases(&hello), (json!(true), json!({ "on": true })));

    // The same word again moves nothing, so no hello: the next thing sent is a command's answer.
    said(&mut a, json!({ "type": "owner_role", "role": "admin" })).await;
    said(&mut a, command("c1", "start_over", json!({}))).await;
    assert_eq!(read_not_alive(&mut a).await["id"], "c1");

    // The login loses admin: off, with why.
    v.settings.gate().record("one", false, Instant::now());
    let hello = read_not_alive(&mut a).await;
    assert_eq!(
        hello["computer"]["releases"]["why"],
        "Off: this computer's GitHub login isn't an admin of octo/game"
    );
    c.stop(Duration::from_secs(1)).await;
    drop(conf);
}

#[tokio::test]
async fn a_project_this_computer_hasnt_got_is_off_and_the_switch_set_false_is_off() {
    let (conf, v) = computer_with_rollout();
    std::fs::write(
        conf.path().join("config.json"),
        json!({ "version": 2, "releases": false, "projects": [{ "id": "one", "githubRepo": "octo/game" }] })
            .to_string(),
    )
    .unwrap();
    let off = v.hello("one").await;
    assert_eq!(releases(&off), (json!(false), json!({ "on": false })));
    let other = v.hello("two").await;
    assert_eq!(other["policy"]["releases"], json!(false));
    assert!(other["computer"]["releases"]["on"] == json!(false));
}
