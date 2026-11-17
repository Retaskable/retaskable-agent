use std::time::Duration;

use super::*;
use crate::secrets::fakes::MemStore;

const BACKLOG: &str = "https://backlog.retaskable.com";

fn config_in(dir: &std::path::Path, json: Value) -> ConfigFile {
    std::fs::write(dir.join("config.json"), json.to_string()).unwrap();
    config::load(dir).unwrap()
}

fn context(tokens: &[(&str, &str)]) -> ComputerContext {
    let keychain: Arc<dyn TokenStore> = Arc::new(MemStore::with(tokens));
    let links = Links::with(
        keychain.clone(),
        reqwest::Client::new(),
        json!({}),
        Arc::new(|_| Ok(())),
        Duration::from_secs(1),
    );
    ComputerContext {
        default_port: 4701,
        build: None,
        keychain,
        links,
    }
}

#[test]
fn bare_address_is_the_origin_or_the_host_and_port_and_nothing_else() {
    let url = "https://user:pw@example.com:8443/a/b?token=1#x";
    assert_eq!(bare_address(url, true), "https://example.com:8443");
    assert_eq!(bare_address(url, false), "example.com:8443");
    assert_eq!(bare_address("http://localhost:8787/", false), "localhost:8787");
    assert_eq!(bare_address("https://retaskable.com/x", false), "retaskable.com");
    assert_eq!(bare_address("not a url", true), "");
    assert_eq!(bare_address("", false), "");
}

/// `ComputerView` (shared/computerView.ts) from a config with everything set: each key, in the TypeScript agent's
/// shapes, and nothing else (`releases`, `lanesAuto` and `usesEngines` are left out, see the module's note).
#[test]
fn the_view_is_every_machine_setting_in_computer_views_shapes() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_in(
        dir.path(),
        json!({
            "version": 2, "lanes": 3, "choreLane": 2, "laneScaling": { "base": 1, "belowUsedPct": 60 },
            "concurrency": 4, "modelConcurrency": { "opus": 2 }, "pollIntervalSec": 20, "port": 4800,
            "models": { "opusAbovePct": 60, "sonnetAbovePct": 30, "pauseBelowPct": 10 },
            "local": { "enabled": true, "model": "qwen", "baseUrl": "http://user:pw@10.0.0.5:8080/v1?k=secret" },
            "budget": { "dailyUsd": 12.5 },
            "cursor": { "models": { "top": "gpt-5" } },
            "engineChoice": { "default": "cursor", "roles": { "review": "local" } },
            "cloud": { "url": "https://staging.retaskable.com/" },
        }),
    );
    let machine: Option<MachineStore> = None;
    let v = view(&cfg, &Env::empty(), machine.as_ref(), None, &["linux".to_string()]);
    assert_eq!(
        Value::Object(v),
        json!({
            "lanes": 3, "choreLane": 2, "laneScaling": { "base": 1, "belowUsedPct": 60.0 },
            "concurrency": 4, "modelConcurrency": { "opus": 2, "sonnet": 4 },
            "pollIntervalSec": 20, "port": 4800,
            "models": { "opusAbovePct": 60, "sonnetAbovePct": 30, "pauseBelowPct": 10 },
            "local": { "enabled": true, "model": "qwen", "server": "10.0.0.5:8080" },
            "dailyUsd": 12.5,
            "cursorModels": { "top": "gpt-5", "implementer": "", "scout": "" },
            "engines": { "default": "cursor", "roles": {
                "planner": "default", "implementer": "default", "review": "local",
                "drafter": "default", "chat": "default", "writer": "default" } },
            "cloud": "https://staging.retaskable.com", "backlog": BACKLOG,
            "tags": { "found": ["linux"], "added": [], "removed": [] },
            "network": false,
        })
    );
}

#[test]
fn an_empty_config_reads_as_the_defaults_and_leaves_the_optional_keys_out() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config::load(dir.path()).unwrap();
    let v = view(&cfg, &Env::empty(), None, Some(&context(&[])), &[]);
    for absent in [
        "laneScaling",
        "choreLane",
        "modelConcurrency",
        "releases",
        "lanesAuto",
        "usesEngines",
    ] {
        assert!(!v.contains_key(absent), "{absent}");
    }
    assert_eq!(v["port"], 4701, "the build's own default port");
    assert_eq!(
        v["local"],
        json!({ "enabled": false, "model": "", "server": "localhost:11436" })
    );
    assert_eq!(v["engines"]["default"], "claude");
    assert_eq!(v["cloud"], "https://retaskable.com");
}

#[test]
fn the_environment_wins_for_the_settings_it_sets_and_tags_and_network_come_from_machine_json() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_in(dir.path(), json!({ "version": 2, "pollIntervalSec": 20 }));
    std::fs::write(
        dir.path().join("machine.json"),
        json!({ "tags": { "added": ["big"], "removed": ["docker"] },
                "network": { "enabled": true, "passwordHash": "h" } })
        .to_string(),
    )
    .unwrap();
    let machine = MachineStore::load(dir.path()).unwrap();
    let env = Env::from_pairs([("RETASKABLE_POLL_SEC", "45"), ("RETASKABLE_PORT", "4999")]);
    let v = view(
        &cfg,
        &env,
        Some(&machine),
        None,
        &["linux".to_string(), "docker".to_string()],
    );
    assert_eq!(
        (v["pollIntervalSec"].clone(), v["port"].clone()),
        (json!(45), json!(4999))
    );
    assert_eq!(
        v["tags"],
        json!({ "found": ["linux", "docker"], "added": ["big"], "removed": ["docker"] })
    );
    assert_eq!(v["network"], true);
    // Off in the file, on through a long enough environment password, and a short one doesn't count.
    let off = MachineStore::load(tempfile::tempdir().unwrap().path()).unwrap();
    let with = |pw: &str| {
        view(
            &cfg,
            &Env::from_pairs([("RETASKABLE_NETWORK_PASSWORD", pw)]),
            Some(&off),
            None,
            &[],
        )
    };
    assert_eq!(with("long enough password")["network"], true);
    assert_eq!(with("short")["network"], false);
}

#[test]
fn links_are_named_by_host_with_their_state_the_backlog_store_last() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config::load(dir.path()).unwrap();
    let ctx = context(&[("https://staging.retaskable.com", "tok"), (BACKLOG, "tok")]);
    let rows = links(&ctx, &cfg, &Env::empty());
    let row = |key: &str, state: &str| json!({ "key": key, "state": state, "kind": "link" });
    assert_eq!(
        rows,
        [
            row("retaskable.com", "not linked"),
            row("staging.retaskable.com", "linked"),
            row("localhost", "not linked"),
            row("backlog store backlog.retaskable.com", "linked"),
        ]
    );
    // An approval waiting on the backlog store says so, whether or not a token is kept.
    ctx.links.set_backlog(Some(crate::link::pending::BacklogWaiting {
        label: "Approve the shared backlog".into(),
        page_url: "https://backlog.retaskable.com/approve".into(),
    }));
    assert_eq!(
        links(&ctx, &cfg, &Env::empty()).last().unwrap(),
        &row("backlog store backlog.retaskable.com", "waiting")
    );
}
