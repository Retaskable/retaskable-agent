//! docs/specs/setup-wizard.md section 2.3: the CI runner takes the code agent's `GH_TOKEN` from the shared data folder
//! when it has none of its own, so one token saved in the wizard covers both programs.
use std::sync::Arc;

use serde_json::{Map, Value, json};

use super::fakes::{FakeHost, MemStore};
use super::*;
use crate::env::Env;

const AGENT_TOKEN: &str = "ghp_PLANTED_agent_token_1234567890";
const CI_TOKEN: &str = "ghp_PLANTED_ci_own_token_1234567890";

fn secrets(root: &std::path::Path, owner: Owner, os: &Arc<MemStore>, host: &Arc<FakeHost>) -> Secrets {
    Secrets::build(owner, root, &Env::empty(), host.clone(), Some(os.clone()), None)
}

fn save(s: &Secrets, name: &str, value: Value) {
    let changes: Map<String, Value> = [(name.to_string(), value)].into_iter().collect();
    s.commit(s.plan(&changes).unwrap()).unwrap();
}

#[test]
fn the_runner_uses_the_agents_token_when_it_has_none_of_its_own() {
    let root = tempfile::tempdir().unwrap();
    let os = Arc::new(MemStore::default());
    let agent = secrets(root.path(), Owner::CodeAgent, &os, &Arc::new(FakeHost::default()));
    save(&agent, "GH_TOKEN", json!(AGENT_TOKEN));

    let host = Arc::new(FakeHost::default());
    let ci = secrets(root.path(), Owner::CiRunner, &os, &host);
    ci.apply();
    assert_eq!(host.all().get("GH_TOKEN").map(String::as_str), Some(AGENT_TOKEN));
    let state = ci.states().into_iter().find(|s| s.name == "GH_TOKEN").unwrap();
    assert!(state.inherited && state.source == Source::Saved);
    assert!(
        !std::fs::read_to_string(Owner::CiRunner.file(root.path()))
            .unwrap_or_default()
            .contains("GH_TOKEN"),
        "nothing is copied into the runner's own file"
    );
}

#[test]
fn the_runners_own_token_wins_and_is_not_inherited() {
    let root = tempfile::tempdir().unwrap();
    let os = Arc::new(MemStore::default());
    let agent = secrets(root.path(), Owner::CodeAgent, &os, &Arc::new(FakeHost::default()));
    save(&agent, "GH_TOKEN", json!(AGENT_TOKEN));
    let host = Arc::new(FakeHost::default());
    let ci = secrets(root.path(), Owner::CiRunner, &os, &host);
    save(&ci, "GH_TOKEN", json!(CI_TOKEN));
    ci.apply();
    assert_eq!(host.all().get("GH_TOKEN").map(String::as_str), Some(CI_TOKEN));
    assert!(
        !ci.states()
            .into_iter()
            .find(|s| s.name == "GH_TOKEN")
            .unwrap()
            .inherited
    );
}

#[test]
fn a_running_runner_follows_the_agents_token_as_it_is_saved_and_removed() {
    let root = tempfile::tempdir().unwrap();
    let os = Arc::new(MemStore::default());
    let agent = secrets(root.path(), Owner::CodeAgent, &os, &Arc::new(FakeHost::default()));
    let host = Arc::new(FakeHost::default());
    let ci = secrets(root.path(), Owner::CiRunner, &os, &host);
    ci.apply();
    assert!(!host.all().contains_key("GH_TOKEN"));
    save(&agent, "GH_TOKEN", json!(AGENT_TOKEN));
    ci.refresh();
    assert_eq!(host.all().get("GH_TOKEN").map(String::as_str), Some(AGENT_TOKEN));
    save(&agent, "GH_TOKEN", Value::Null);
    ci.refresh();
    assert!(!host.all().contains_key("GH_TOKEN"));
}

#[test]
fn the_agent_never_takes_the_runners_token_and_the_chat_agent_none() {
    let root = tempfile::tempdir().unwrap();
    let os = Arc::new(MemStore::default());
    let ci = secrets(root.path(), Owner::CiRunner, &os, &Arc::new(FakeHost::default()));
    save(&ci, "GH_TOKEN", json!(CI_TOKEN));
    let host = Arc::new(FakeHost::default());
    let agent = secrets(root.path(), Owner::CodeAgent, &os, &host);
    agent.apply();
    assert!(!host.all().contains_key("GH_TOKEN"));
}
