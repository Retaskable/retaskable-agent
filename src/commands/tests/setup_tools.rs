use std::path::PathBuf;
use std::sync::Arc;

use super::super::*;
use super::{cmd, data_dir, view};
use crate::setup_check::{SetupCheck, SetupCheckRunner, SetupChecks};
use crate::setup_tools::{Deps, Ran, Setup};

fn checks(installed: bool, signed_in: bool) -> SetupChecks {
    let ck = |ok: bool| SetupCheck {
        ok: Some(ok),
        detail: String::new(),
        fix: None,
    };
    let mut c = SetupChecks::all_unknown();
    c.git = ck(installed);
    c.gh_installed = ck(installed);
    c.gh = ck(signed_in);
    c.claude = ck(installed);
    c.claude_signed_in = ck(signed_in);
    c
}

/// A view whose installs run a fake program that exits 0, on Linux, with the checks as given.
fn setup_view(installed: bool, signed_in: bool) -> AgentView {
    let runner = Arc::new(SetupCheckRunner::with_probe(Box::new(move || {
        let c = checks(installed, signed_in);
        Box::pin(async move { c })
    })));
    *runner.shared().lock().unwrap() = Some(checks(installed, signed_in));
    let deps = Deps {
        run: Arc::new(|_, _, _| Ran {
            code: Some(0),
            ..Ran::default()
        }),
        download: Arc::new(|_| Ok(b"script".to_vec())),
        extract: Arc::new(|_, _| Ok(())),
        open: Arc::new(|_| {}),
        ask: Arc::new(|| None),
        wait: Arc::new(|_| {}),
        refresh: Arc::new(|| None),
        use_gh: Arc::new(|_| {}),
        use_git: Arc::new(|_| {}),
        use_local_bin: Arc::new(|_| {}),
        log: Arc::new(|_| {}),
    };
    let data = std::env::temp_dir().join("retaskable-commands-test-setup");
    let mut v = view();
    v.setup = Some(Setup::with(
        runner,
        Default::default(),
        data,
        PathBuf::new(),
        "linux".into(),
        "x86_64".into(),
        deps,
    ));
    v
}

fn ask(v: &AgentView, project: &str, name: &str, args: Value) -> Value {
    run(
        &cmd(name, args),
        project,
        &mut Seen::default(),
        v,
        &|_: i32| {},
        &data_dir(),
    )
}

#[test]
fn without_a_setup_both_are_refused() {
    let v = view();
    assert_eq!(
        ask(&v, "one", "install_tool", json!({ "tool": "gh" }))["reason"],
        "This agent can't install tools from retaskable.com."
    );
    assert_eq!(
        ask(&v, "one", "tool_sign_in", json!({ "tool": "gh" }))["reason"],
        "This agent can't sign in from retaskable.com."
    );
}

#[test]
fn only_a_fixed_tool_name_and_nothing_else_is_taken() {
    let v = setup_view(false, false);
    for args in [
        json!({}),
        json!({ "tool": "node" }),
        json!({ "tool": "gh", "cmd": "rm" }),
        json!({ "tool": 1 }),
        Value::Null,
    ] {
        let a = ask(&v, "one", "install_tool", args);
        assert_eq!(a["type"], "nack");
        assert_eq!(a["reason"], "install_tool takes only { tool: git | gh | claude }.");
    }
    assert_eq!(
        ask(&v, "one", "tool_sign_in", json!({ "tool": "git" }))["reason"],
        "tool_sign_in takes only { tool: gh | claude }."
    );
}

#[tokio::test]
async fn an_installed_tool_is_refused_at_once() {
    let v = setup_view(true, true);
    let a = ask(&v, "one", "install_tool", json!({ "tool": "claude" }));
    assert_eq!(
        (a["type"].as_str(), a["reason"].as_str()),
        (Some("nack"), Some("Already installed."))
    );
}

#[tokio::test]
async fn an_install_is_pending_then_acks_once_done_even_before_setup() {
    for project in ["one", ""] {
        let v = setup_view(false, false);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        v.item_commands.set_out(project, Some(tx));
        let a = ask(&v, project, "install_tool", json!({ "tool": "claude" }));
        assert_eq!(a, json!({ "type": PENDING, "id": "c1" }));
        let done = tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(done, json!({ "type": "ack", "id": "c1" }));
        assert_eq!(
            v.setup.as_ref().unwrap().activity()["last"]["detail"],
            "Claude Code is installed."
        );
    }
}

#[tokio::test]
async fn a_sign_in_needs_the_tool_first_and_refuses_when_already_signed_in() {
    let v = setup_view(false, false);
    assert_eq!(
        ask(&v, "one", "tool_sign_in", json!({ "tool": "gh" }))["reason"],
        "Install it first."
    );
    let v = setup_view(true, true);
    assert_eq!(
        ask(&v, "", "tool_sign_in", json!({ "tool": "gh" }))["reason"],
        "Already signed in."
    );
}

#[test]
fn hello_carries_the_setup_activity_even_when_empty() {
    let v = setup_view(false, false);
    let hello = tokio::runtime::Runtime::new().unwrap().block_on(v.hello("one"));
    // Nothing running or finished (a Linux adds its `os`).
    let setup = hello["setup"].as_object().unwrap();
    assert!(setup.keys().all(|k| k == "os"), "{setup:?}");
    assert!(view().setup.is_none());
}
