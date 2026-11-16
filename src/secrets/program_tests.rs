//! The CI runner's and the Chat Agent's own secrets: each in its own `settings.json`, all sealed under the one key of
//! the data folder, the page saving them without touching its own environment, the program picking a save up live.
use std::sync::Arc;

use serde_json::{Map, Value, json};

use super::fakes::{FakeHost, MemStore};
use super::*;
use crate::env::Env;

const TOKEN: &str = "ghp_PLANTED_ci_token_1234567890";

struct Rig {
    root: tempfile::TempDir,
    os: Arc<MemStore>,
}

impl Rig {
    fn new() -> Self {
        Self {
            root: tempfile::tempdir().unwrap(),
            os: Arc::new(MemStore::default()),
        }
    }

    /// The program itself, with its own fake environment.
    fn program(&self, owner: Owner, host: Arc<FakeHost>) -> Secrets {
        Secrets::build(
            owner,
            self.root.path(),
            &Env::empty(),
            host,
            Some(self.os.clone()),
            None,
        )
    }

    /// The settings page's handle on a program's secrets, `env_set` from that program's status file.
    fn page(&self, owner: Owner, env_set: &[&str]) -> Secrets {
        let env_set: Vec<String> = env_set.iter().map(|s| s.to_string()).collect();
        Secrets::for_page(owner, self.root.path(), &Env::empty(), Some(self.os.clone()), &env_set)
    }

    fn file(&self, owner: Owner) -> String {
        std::fs::read_to_string(owner.file(self.root.path())).unwrap_or_default()
    }
}

fn save(secrets: &Secrets, pairs: &[(&str, Value)]) {
    let changes: Map<String, Value> = pairs.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect();
    secrets.commit(secrets.plan(&changes).unwrap()).unwrap();
}

#[test]
fn each_program_keeps_its_secrets_in_its_own_file_and_takes_only_its_own_names() {
    let rig = Rig::new();
    let ci = rig.program(Owner::CiRunner, Arc::new(FakeHost::default()));
    save(&ci, &[("GH_TOKEN", json!(TOKEN))]);
    let text = rig.file(Owner::CiRunner);
    assert!(text.contains("\"secrets\"") && text.contains("v1:") && !text.contains(TOKEN));
    assert!(
        Owner::CiRunner
            .file(rig.root.path())
            .ends_with(std::path::Path::new("ci").join("settings.json"))
    );
    assert_eq!(
        rig.file(Owner::CodeAgent),
        "",
        "the code agent's config.json is not touched"
    );
    assert!(
        ci.plan(&json!({"CLAUDE_CODE_OAUTH_TOKEN": "x"}).as_object().unwrap().clone())
            .is_err()
    );
    let chat = rig.program(Owner::ChatAgent, Arc::new(FakeHost::default()));
    assert!(
        chat.plan(&json!({"GH_TOKEN": "x"}).as_object().unwrap().clone())
            .is_err()
    );
    save(&chat, &[("ANTHROPIC_API_KEY", json!("sk-ant-planted-key"))]);
    assert!(rig.file(Owner::ChatAgent).contains("v1:"));
    assert!(!rig.file(Owner::ChatAgent).contains("planted"));
}

#[test]
fn all_programs_share_the_one_data_key_of_the_data_folder() {
    let rig = Rig::new();
    save(
        &rig.program(Owner::CiRunner, Arc::new(FakeHost::default())),
        &[("GH_TOKEN", json!(TOKEN))],
    );
    save(
        &rig.program(Owner::ChatAgent, Arc::new(FakeHost::default())),
        &[("CLAUDE_CODE_OAUTH_TOKEN", json!("tok"))],
    );
    assert_eq!(rig.os.names().len(), 1, "one key, whichever program saved first");
    let host = Arc::new(FakeHost::default());
    let ci = rig.program(Owner::CiRunner, host.clone());
    ci.apply();
    assert_eq!(host.get("GH_TOKEN").as_deref(), Some(TOKEN));
}

#[test]
fn a_saved_secret_beats_the_programs_environment_for_each_program_and_is_reported_for_the_status_file() {
    for (owner, name) in [
        (Owner::CiRunner, "GH_TOKEN"),
        (Owner::ChatAgent, "ANTHROPIC_API_KEY"),
        (Owner::ChatAgent, "CLAUDE_CODE_OAUTH_TOKEN"),
    ] {
        let rig = Rig::new();
        save(
            &rig.program(owner, Arc::new(FakeHost::default())),
            &[(name, json!(TOKEN))],
        );
        let host = Arc::new(FakeHost::with(&[(name, "from-the-environment")]));
        let program = rig.program(owner, host.clone());
        program.apply();
        assert_eq!(host.get(name).as_deref(), Some(TOKEN), "{owner:?} {name}");
        assert_eq!(
            program.env_names(),
            [name],
            "the status file still says the environment set it"
        );
        assert!(
            program
                .plan(&json!({ name: "other" }).as_object().unwrap().clone())
                .is_ok()
        );
        // Deleting the saved one goes back to what the process started with.
        save(&program, &[(name, Value::Null)]);
        assert_eq!(host.get(name).as_deref(), Some("from-the-environment"));
    }
}

#[test]
fn the_page_saves_a_programs_secret_without_touching_its_own_environment_and_shows_the_saved_one_first() {
    let rig = Rig::new();
    let page = rig.page(Owner::CiRunner, &["AGENT_CI_SLOTS"]);
    save(&page, &[("GH_TOKEN", json!(TOKEN))]);
    assert!(rig.file(Owner::CiRunner).contains("v1:"));
    let state = page.states().remove(0);
    assert_eq!(
        (state.source, state.fingerprint, state.env_set),
        (Source::Saved, Some(fingerprint(TOKEN)), false)
    );
    // The program's own environment sets it too: still the saved one, and a reset goes back to the environment's.
    let with_env = rig.page(Owner::CiRunner, &["GH_TOKEN"]);
    let state = with_env.states().remove(0);
    assert_eq!((state.source, state.env_set), (Source::Saved, true));
    save(&with_env, &[("GH_TOKEN", Value::Null)]);
    let state = with_env.states().remove(0);
    assert_eq!((state.source, state.env_set), (Source::Env, true));
    // Nothing saved, nothing from the environment: unset, and saving is open.
    let unset = rig.page(Owner::ChatAgent, &[]);
    assert_eq!(unset.states()[0].source, Source::Unset);
    assert!(
        unset
            .plan(&json!({"ANTHROPIC_API_KEY": "k"}).as_object().unwrap().clone())
            .is_ok()
    );
}

#[test]
fn a_computer_with_no_key_store_cannot_save_and_nothing_is_written() {
    let rig = Rig::new();
    let page = Secrets::for_page(Owner::CiRunner, rig.root.path(), &Env::empty(), None, &[]);
    assert!(page.save_problem().is_some());
    assert!(
        page.plan(&json!({"GH_TOKEN": TOKEN}).as_object().unwrap().clone())
            .is_err()
    );
    assert_eq!(rig.file(Owner::CiRunner), "");
}

#[test]
fn a_running_program_picks_up_a_save_from_the_page_and_a_clear() {
    let rig = Rig::new();
    let host = Arc::new(FakeHost::default());
    let ci = rig.program(Owner::CiRunner, host.clone());
    ci.apply();
    assert_eq!(host.get("GH_TOKEN"), None);
    save(&rig.page(Owner::CiRunner, &[]), &[("GH_TOKEN", json!(TOKEN))]);
    ci.refresh();
    assert_eq!(host.get("GH_TOKEN").as_deref(), Some(TOKEN));
    assert_eq!(host.git_logins(), 1);
    ci.refresh();
    assert_eq!(host.git_logins(), 1, "nothing changed: nothing done");
    save(&rig.page(Owner::CiRunner, &[]), &[("GH_TOKEN", Value::Null)]);
    ci.refresh();
    assert_eq!(host.get("GH_TOKEN"), None);
}

#[test]
fn a_refresh_puts_a_save_over_the_environment_and_a_delete_goes_back_to_it() {
    let rig = Rig::new();
    let host = Arc::new(FakeHost::with(&[("GH_TOKEN", "from-the-environment")]));
    let ci = rig.program(Owner::CiRunner, host.clone());
    ci.apply();
    save(&rig.page(Owner::CiRunner, &[]), &[("GH_TOKEN", json!(TOKEN))]);
    ci.refresh();
    assert_eq!(host.get("GH_TOKEN").as_deref(), Some(TOKEN));
    save(&rig.page(Owner::CiRunner, &[]), &[("GH_TOKEN", Value::Null)]);
    ci.refresh();
    assert_eq!(host.get("GH_TOKEN").as_deref(), Some("from-the-environment"));
}
