use super::*;

fn plan(event: &'static str) -> Plan<'static> {
    Plan {
        computer: "My PC.local",
        repo: "Acme/Web_App",
        sha: "0123456789abcdef",
        event,
        pr: 7,
        cpus: 4,
    }
}

fn has(args: &[String], pair: [&str; 2]) -> bool {
    args.windows(2).any(|w| w[0] == pair[0] && w[1] == pair[1])
}

#[test]
fn names_are_the_runners() {
    assert_eq!(slug("My PC.local"), "my-pc-local");
    assert_eq!(
        name_prefix("My PC.local", "Acme/Web_App"),
        "agent-ci-my-pc-local-acme-web_app--"
    );
    assert_eq!(
        plan("pull_request").name(Step::Check),
        "agent-ci-my-pc-local-acme-web_app--01234567-pull_request-check"
    );
    assert_eq!(
        plan("push").name(Step::Warm),
        "agent-ci-my-pc-local-acme-web_app--01234567-push-warm"
    );
}

#[test]
fn the_fetch_holds_the_token_by_name_only() {
    let a = plan("pull_request").fetch_args();
    assert_eq!(&a[..5], ["run", "--rm", "--label", GROUP_LABEL, "--name"]);
    assert!(has(&a, ["-v", "agent-ci-mirror-acme-web_app:/m"]));
    assert!(has(&a, ["-e", "T"]) && has(&a, ["-e", "REF=refs/pull/7/head"]) && has(&a, ["-e", "SHA=0123456789abcdef"]));
    assert!(a.iter().all(|s| !s.starts_with("T=")));
    let n = a.len();
    assert_eq!(
        (a[n - 4].as_str(), a[n - 3].as_str(), a[n - 2].as_str()),
        (DEFAULT_IMAGE, "sh", "-c")
    );
    assert!(a[n - 1].contains("github.com/Acme/Web_App.git"));
    assert!(has(&plan("push").fetch_args(), ["-e", "REF=refs/heads/main"]));
}

#[test]
fn the_fetch_script_is_the_runners() {
    let s = fetch_script("o/r");
    assert!(s.starts_with("set -e\ngit config --global safe.directory \"*\"\n"));
    assert!(s.contains("[ -d /m/objects ] || git clone -q --bare \"https://x-access-token:$T@github.com/o/r.git\" /m"));
    assert!(s.contains("for i in 1 2 3 4 5; do git -C /m fetch -q --no-tags origin"));
    assert!(s.contains("sleep 3; done; [ -n \"$ok\" ] || exit 1"));
    assert!(s.ends_with("echo \"AGENT_CI_IMAGE $img\""));
}

#[test]
fn the_warm_up_has_no_token_and_the_mirror_read_only() {
    let a = plan("pull_request").warm_args();
    assert!(has(&a, ["--cpus", "4"]) && has(&a, ["-v", "agent-ci-mirror-acme-web_app:/m:ro"]));
    assert!(has(&a, ["-v", "agent-ci-deps:/deps"]) && has(&a, ["-v", "agent-ci-npm:/npm-cache"]));
    assert!(has(&a, ["-e", "CI=true"]) && has(&a, ["-e", "npm_config_cache=/npm-cache"]));
    assert!(!a.contains(&"T".to_string()));
    let script = a.last().unwrap();
    assert!(script.ends_with("git clone -q /m /w\ncd /w\ngit checkout -q main\nbash .agent-ci/warm.sh"));
}

#[test]
fn the_check_has_the_runners_mounts_and_environment() {
    let a = plan("pull_request").check_args("rust:1", "[\"a.rs\"]", Some("Release flag: x"));
    assert!(has(&a, ["--cpus", "4"]) && has(&a, ["--shm-size", "1g"]));
    for v in [
        "agent-ci-mirror-acme-web_app:/m:ro",
        "agent-ci-npm:/npm-cache",
        "agent-ci-deps:/deps:ro",
        "agent-ci-cargo:/usr/local/cargo/registry",
    ] {
        assert!(has(&a, ["-v", v]), "{v}");
    }
    for e in [
        "CI=true",
        "npm_config_cache=/npm-cache",
        "npm_config_prefer_offline=true",
        "CI_PIPELINE_EVENT=pull_request",
        "CI_PIPELINE_FILES=[\"a.rs\"]",
        "CI_COMMIT_TARGET_BRANCH=main",
        "SHA=0123456789abcdef",
        "CI_CARD_LINE=Release flag: x",
    ] {
        assert!(has(&a, ["-e", e]), "{e}");
    }
    assert!(!a.iter().any(|s| s.starts_with("CI_CARD_UNREADABLE")));
    let n = a.len();
    assert_eq!(
        (a[n - 4].as_str(), a[n - 3].as_str(), a[n - 2].as_str()),
        ("rust:1", "bash", "-c")
    );
    // No card: an empty line, and when the body couldn't be read the check is told.
    let none = plan("pull_request").check_args("rust:1", "[]", None);
    assert!(has(&none, ["-e", "CI_CARD_LINE="]) && has(&none, ["-e", "CI_CARD_UNREADABLE=1"]));
}

#[test]
fn a_pull_request_merges_main_and_a_push_does_not() {
    let pr = check_script("pull_request");
    let push = check_script("push");
    assert!(pr.starts_with("PS4='+ $(printf \"%(%T)T\" -1) '\nset -ex\n"));
    assert!(pr.contains("git fetch -q origin refs/ci/$SHA\ngit checkout -q FETCH_HEAD\ngit -c user.name=ci -c user.email=ci@localhost merge -q --no-edit origin/main || { echo \"error: conflicts with main; merge main into the branch\"; exit 1; }\nbash .agent-ci/checks.sh pull_request"));
    assert!(!push.contains("merge -q") && push.ends_with("bash .agent-ci/checks.sh push"));
}

#[test]
fn the_image_is_read_from_the_fetch_log_and_checked() {
    assert_eq!(image_from("cloning\n").unwrap(), DEFAULT_IMAGE);
    assert_eq!(
        image_from("x\r\nAGENT_CI_IMAGE rust:1-bookworm \r\n").unwrap(),
        "rust:1-bookworm"
    );
    assert_eq!(image_from("AGENT_CI_IMAGE   \n").unwrap(), DEFAULT_IMAGE);
    assert_eq!(
        image_from("AGENT_CI_IMAGE ghcr.io/o/i@sha256:ab\n").unwrap(),
        "ghcr.io/o/i@sha256:ab"
    );
    for bad in ["-rm", "a b", "x;y", "$(id)"] {
        let e = image_from(&format!("AGENT_CI_IMAGE {bad}\n")).unwrap_err();
        assert_eq!(e, format!(".agent-ci/image is not an image name: {bad}"));
    }
    let long = format!("a{}", "b".repeat(200));
    let e = image_from(&format!("AGENT_CI_IMAGE {long}")).unwrap_err();
    assert_eq!(e.chars().count(), ".agent-ci/image is not an image name: ".len() + 80);
}
