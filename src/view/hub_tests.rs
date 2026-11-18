//! What this agent sends the hub, checked against what the hub accepts (`connector::hub_contract`), not against
//! itself: the busy snapshot has every block a live computer fills, so a wrong shape in any of them fails here.
use std::path::Path;

use serde_json::{Value, json};

use super::*;
use crate::connector::hub_contract::{MAX_STATE_BYTES, check};
use crate::orchestrator::live::{HeldItem, HeldLane};

fn project() -> ProjectView {
    ProjectView {
        id: "a".into(),
        github_repo: "o/a".into(),
        backlog_path: "backlog".into(),
        repo_path: PathBuf::new(),
        worktrees_dir: PathBuf::new(),
        base_branch: "main".into(),
        play_command: String::new(),
        phone: None,
        organisation_id: None,
        code_host: "github".into(),
        code_host_ref: String::new(),
    }
}

fn view() -> AgentView {
    crate::test_support::agent_view(vec!["windows".into()], vec![project()], "1.2.3", 3)
}

/// A view whose loop holds two lanes, an item and a chore, with a restart waiting and an anomaly open.
fn busy() -> AgentView {
    let v = view();
    v.live.register("a");
    v.live.set_concurrency(3);
    v.live.set_repo_id("a", Some(1_433_581_756));
    v.live.ticked(
        "a",
        false,
        "t".into(),
        vec![HeldLane {
            lane: 1,
            item_id: "i1".into(),
            title: None,
            branch: Some("retaskable/i1-s1".into()),
            playing: true,
        }],
        "d".into(),
    );
    v.live.set_items(
        "a",
        vec![HeldItem {
            id: "i1".into(),
            title: "T".into(),
            state: "building".into(),
            lane: Some(1),
            branch: "retaskable/i1-s1".into(),
            pr_url: Some("https://github.com/o/a/pull/1".into()),
            attempt: 1,
            chore: true,
            expert: None,
        }],
    );
    v.restart.ask(crate::restart::Why {
        at: 1_700_000_000_000,
        by: "ann".into(),
        reason: "asked".into(),
    });
    v
}

#[test]
fn the_snapshot_passes_what_the_hub_checks_before_setup_while_idle_and_while_busy() {
    let dir = tempfile::tempdir().unwrap();
    let idle = view();
    for (v, project) in [(&idle, ""), (&idle, "a"), (&busy(), "a")] {
        let snap = v.snapshot(project, dir.path());
        check(&snap).unwrap_or_else(|e| panic!("{e}: {snap}"));
        assert!(snap.to_string().len() < MAX_STATE_BYTES);
    }
}

#[tokio::test]
async fn the_hello_passes_what_the_hub_checks_with_and_without_checks_and_a_project() {
    let v = view();
    for project in ["", "a", "unknown"] {
        let hello = v.hello(project).await;
        check(&hello).unwrap_or_else(|e| panic!("{e}: {hello}"));
    }
    v.checks.run_now().await;
    let hello = v.hello("a").await;
    check(&hello).unwrap_or_else(|e| panic!("{e}: {hello}"));
    assert!(hello["checks"]["node"].is_object());
}

/// A hello with `computer` filled from a config, machine settings and links, as a running agent sends it: the full
/// `ComputerView` passes, and each way a key can be wrong fails.
#[tokio::test]
async fn the_hello_computer_passes_in_full_and_a_wrong_key_or_value_fails() {
    use crate::computer_view::ComputerContext;
    use crate::hello_settings::HelloSettings;
    use crate::link::{TokenStore, pending::Links};
    use crate::secrets::fakes::MemStore;

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.json"),
        json!({ "version": 2, "choreLane": 2, "laneScaling": { "base": 1, "belowUsedPct": 60 },
                "modelConcurrency": { "opus": 2, "sonnet": 3 }, "local": { "enabled": true, "model": "m" },
                "engineChoice": { "default": "codex", "roles": { "chat": "local" } } })
        .to_string(),
    )
    .unwrap();
    let keychain: Arc<dyn TokenStore> = Arc::new(MemStore::with(&[("https://retaskable.com", "t")]));
    let links = Links::new(keychain.clone(), reqwest::Client::new(), &crate::env::Env::empty(), "1");
    let settings = HelloSettings::new(
        dir.path().to_path_buf(),
        crate::env::Env::from_pairs([("RETASKABLE_PORT", "4801")]),
    );
    settings.set_context(ComputerContext {
        default_port: 4700,
        build: None,
        keychain,
        links,
    });
    let mut v = view();
    v.settings = settings;
    let hello = v.hello("a").await;
    check(&hello).unwrap_or_else(|e| panic!("{e}: {hello}"));
    let computer = &hello["computer"];
    assert_eq!(computer["port"], 4801);
    assert!(
        computer["secrets"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["kind"] == "link" && s["state"] == "linked")
    );

    let wrong = |edit: &dyn Fn(&mut Value)| {
        let mut h = hello.clone();
        edit(&mut h["computer"]);
        check(&h).unwrap_err()
    };
    assert!(
        wrong(&|c| c["poll"] = json!(5)).contains("computer.poll"),
        "an unknown key"
    );
    assert!(
        wrong(&|c| drop(c.as_object_mut().unwrap().remove("port"))).contains("port"),
        "a missing key"
    );
    assert!(wrong(&|c| c["port"] = json!("4801")).contains("computer.port"));
    assert!(wrong(&|c| c["local"]["servers"] = json!("x")).contains("servers"));
    assert!(wrong(&|c| c["engines"]["roles"]["chat"] = json!("gemini")).contains("chat"));
    assert!(wrong(&|c| c["tags"]["found"] = json!("linux")).contains("tags.found"));
    assert!(wrong(&|c| c["network"] = json!("no")).contains("computer.network"));
    assert!(wrong(&|c| c["secrets"][0]["kind"] = json!("file")).contains("secret.kind"));
}

#[test]
fn the_busy_snapshot_names_each_block_the_hub_keeps() {
    let snap: Value = busy().snapshot("a", Path::new("."));
    let state = &snap["state"];
    for key in [
        "orchestrator",
        "usage",
        "milestones",
        "items",
        "sessions",
        "lanes",
        "mainPlaying",
        "drafts",
        "asks",
    ] {
        assert!(state.get(key).is_some(), "{key}");
    }
    assert_eq!(state["items"][0]["id"], json!("i1"));
}

#[test]
fn a_patch_and_the_messages_of_the_other_jobs_pass_what_the_hub_checks() {
    check(
        &json!({ "type": "patch", "patch": { "orchestrator": { "status": "paused" },
        "removed": { "items": ["i1"], "sessions": [], "drafts": [] } } }),
    )
    .unwrap();
    check(&json!({ "type": "ack", "id": "c1" })).unwrap();
    check(&json!({ "type": "pending", "id": "c1" })).unwrap();
    // A verdict as `jobs::vet` serialises it, with the `ref` the hub link adds.
    for verdict in [
        crate::jobs::vet::SuggestionVerdict::Idea,
        crate::jobs::vet::SuggestionVerdict::Duplicate,
        crate::jobs::vet::SuggestionVerdict::Bin,
        crate::jobs::vet::SuggestionVerdict::Ask,
    ] {
        let v = crate::jobs::vet::Verdict {
            id: 3,
            verdict,
            why: Some("w".into()),
            matches: Some("m".into()),
            question: Some("q".into()),
        };
        let mut msg = serde_json::to_value(&v).unwrap();
        msg["type"] = json!("suggestion_verdict");
        msg["ref"] = json!("ask-1");
        check(&msg).unwrap_or_else(|e| panic!("{e}: {msg}"));
    }
    check(&json!({ "type": "suggestions_ask", "ref": "ask-1" })).unwrap();
}

#[test]
fn a_nack_with_a_long_reason_is_cut_to_what_the_hub_takes_instead_of_being_dropped_whole() {
    let long = crate::commands::nack("c1", "é".repeat(5000));
    assert!(check(&long).is_err(), "the hub caps a nack's reason at 2000");
    let fit = crate::connector::on_the_wire(long);
    check(&fit).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(fit["id"], json!("c1"));
    // An astral character is two units to the hub's count.
    let astral = crate::connector::on_the_wire(crate::commands::nack("c1", "😀".repeat(1500)));
    check(&astral).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        crate::connector::on_the_wire(crate::commands::ack("c1")),
        json!({ "type": "ack", "id": "c1" })
    );
}

#[test]
fn the_snapshot_names_the_projects_repo_so_the_hub_finds_its_backlog() {
    // Without `repoId` the hub can't key the project's store backlog and refuses every store command with "Reading
    // the shared backlog" (storeKeyOf).
    let snap: Value = busy().snapshot("a", Path::new("."));
    assert_eq!(snap["state"]["repoId"], json!(1_433_581_756u64));
    check(&snap).unwrap();
    // Not read yet: no key, and a later None doesn't forget a known id.
    let v = view();
    v.live.register("a");
    assert!(v.snapshot("a", Path::new("."))["state"].get("repoId").is_none());
    v.live.set_repo_id("a", Some(7));
    v.live.set_repo_id("a", None);
    assert_eq!(v.snapshot("a", Path::new("."))["state"]["repoId"], json!(7));
}

/// The hello says which organisation the project goes in, so the hub attaches it there and not in the person's oldest:
/// the shared backlog's record wins over config.json's, and none is sent while neither is known.
#[tokio::test]
async fn the_hello_carries_the_organisation_and_where_the_code_lives_when_known() {
    let v = view();
    let hello = v.hello("a").await;
    check(&hello).unwrap_or_else(|e| panic!("{e}: {hello}"));
    assert_eq!(hello["project"]["codeHost"], "github");
    assert!(hello["project"].get("organisationId").is_none(), "{hello}");

    v.item_commands.organisations.set("a", Some(3));
    let hello = v.hello("a").await;
    check(&hello).unwrap_or_else(|e| panic!("{e}: {hello}"));
    assert_eq!(hello["project"]["organisationId"], 3);

    // config.json's, when the store names none; the store's when both do.
    let mut p = project();
    p.organisation_id = Some(1);
    let v = crate::test_support::agent_view(vec![], vec![p], "1.2.3", 3);
    assert_eq!(v.hello("a").await["project"]["organisationId"], 1);
    v.item_commands.organisations.set("a", Some(3));
    assert_eq!(v.hello("a").await["project"]["organisationId"], 3);
}
