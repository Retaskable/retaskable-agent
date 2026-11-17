//! Variables a person adds beyond a program's known secrets (agent-settings.md section 3.3): any name the rules allow,
//! sealed like a known secret (same map, same key, the name as associated data) and applied the same way.
use std::sync::Arc;

use serde_json::{Map, Value, json};

use super::fakes::{FakeHost, MemStore};
use super::*;
use crate::env::Env;

const VALUE: &str = "PLANTED value with spaces 12345";

struct Rig {
    dir: tempfile::TempDir,
    os: Arc<MemStore>,
}

impl Rig {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
            os: Arc::new(MemStore::default()),
        }
    }

    fn program(&self, host: &Arc<FakeHost>) -> Secrets {
        Secrets::build(
            Owner::CodeAgent,
            self.dir.path(),
            &Env::empty(),
            host.clone(),
            Some(self.os.clone()),
            None,
        )
    }

    fn text(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("config.json")).unwrap_or_default()
    }
}

fn map(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect()
}

fn save(s: &Secrets, pairs: &[(&str, Value)]) {
    s.commit(s.plan(&map(pairs)).unwrap()).unwrap();
}

fn refused(s: &Secrets, pairs: &[(&str, Value)]) -> String {
    match s.plan(&map(pairs)) {
        Err(Refused(why)) => why,
        Ok(_) => panic!("{pairs:?} was accepted"),
    }
}

#[test]
fn a_custom_variable_is_sealed_with_its_name_and_put_in_the_environment() {
    let rig = Rig::new();
    let host = Arc::new(FakeHost::default());
    let s = rig.program(&host);
    save(&s, &[("MY_API_KEY", json!(format!("  {VALUE}  ")))]);
    assert!(rig.text().contains("\"MY_API_KEY\": \"v1:"), "{}", rig.text());
    assert!(!rig.text().contains("PLANTED"));
    assert_eq!(
        host.get("MY_API_KEY").as_deref(),
        Some(VALUE),
        "trimmed, applied to children"
    );
    assert_eq!(host.get("RETASKABLE_SAVED_SECRETS").as_deref(), Some("MY_API_KEY"));
    // Sealed under its name: the same ciphertext under another name doesn't open.
    let stored: Value = serde_json::from_str(&rig.text()).unwrap();
    let sealed = stored["secrets"]["MY_API_KEY"].as_str().unwrap().to_string();
    let key = Keys::open(rig.dir.path(), &Env::empty(), Some(rig.os.clone()))
        .for_reading()
        .unwrap()
        .unwrap();
    assert_eq!(open(&key, "MY_API_KEY", &sealed).as_deref(), Ok(VALUE));
    assert!(open(&key, "OTHER_KEY", &sealed).is_err());
}

#[test]
fn a_saved_custom_variable_wins_over_the_environment_and_a_restart_applies_it() {
    let rig = Rig::new();
    save(&rig.program(&Arc::new(FakeHost::default())), &[("FOO", json!("saved"))]);
    let host = Arc::new(FakeHost::with(&[("FOO", "from-env")]));
    let s = rig.program(&host);
    s.apply();
    assert_eq!(host.get("FOO").as_deref(), Some("saved"));
    let row = s.states().into_iter().find(|r| r.name == "FOO").unwrap();
    assert!(row.def.is_none() && row.source == Source::Saved && row.env_set);
    // Deleting goes back to the environment's value from start (Reset).
    save(&s, &[("FOO", Value::Null)]);
    assert_eq!(host.get("FOO").as_deref(), Some("from-env"));
    assert!(
        s.states().iter().all(|r| r.name != "FOO"),
        "a custom row goes with its value"
    );
    assert!(!rig.text().contains("FOO"));
}

#[test]
fn deleting_a_custom_variable_with_no_environment_value_removes_it() {
    let rig = Rig::new();
    let host = Arc::new(FakeHost::default());
    let s = rig.program(&host);
    save(&s, &[("FOO", json!("x")), ("BAR", json!("y"))]);
    save(&s, &[("FOO", Value::Null)]);
    assert_eq!(host.get("FOO"), None);
    assert_eq!(host.get("BAR").as_deref(), Some("y"));
    assert_eq!(host.get("RETASKABLE_SAVED_SECRETS").as_deref(), Some("BAR"));
}

#[test]
fn the_list_has_known_secrets_then_custom_names_with_fingerprints_and_never_values() {
    let rig = Rig::new();
    let s = rig.program(&Arc::new(FakeHost::default()));
    save(&s, &[("ZED", json!(VALUE)), ("ALPHA", json!("a"))]);
    let rows = s.states();
    let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names[..6], SECRETS.map(|d| d.name));
    assert_eq!(names[6..], ["ALPHA", "ZED"]);
    let zed = rows.iter().find(|r| r.name == "ZED").unwrap();
    assert_eq!(zed.fingerprint.as_deref(), Some(fingerprint(VALUE).as_str()));
    let listed = cli::run_with(&s, Owner::CodeAgent, &["list".to_string()], |_| unreachable!()).unwrap();
    assert!(listed.contains(&format!("ZED  saved  {}", fingerprint(VALUE))) && !listed.contains("PLANTED"));
}

#[test]
fn a_custom_variable_deleted_by_another_process_leaves_the_environment_on_refresh() {
    let rig = Rig::new();
    let host = Arc::new(FakeHost::default());
    let running = rig.program(&host);
    running.apply();
    // The settings page (another process) saves one, then deletes it.
    let page = Secrets::for_page(
        Owner::CodeAgent,
        rig.dir.path(),
        &Env::empty(),
        Some(rig.os.clone()),
        &[],
    );
    save(&page, &[("FOO", json!("x"))]);
    running.refresh();
    assert_eq!(host.get("FOO").as_deref(), Some("x"));
    save(&page, &[("FOO", Value::Null)]);
    running.refresh();
    assert_eq!(host.get("FOO"), None);
    assert_eq!(host.get("RETASKABLE_SAVED_SECRETS"), None);
}

#[test]
fn a_name_the_rules_refuse_in_a_hand_edited_file_is_never_applied() {
    let rig = Rig::new();
    let host = Arc::new(FakeHost::default());
    let s = rig.program(&host);
    save(&s, &[("FOO", json!("x"))]);
    // Rewrite the file with FOO's ciphertext under a refused name, which opens only under its own name anyway: not applied.
    let mut cfg: Value = serde_json::from_str(&rig.text()).unwrap();
    let sealed = cfg["secrets"]["FOO"].clone();
    cfg["secrets"]["PATH"] = sealed;
    std::fs::write(rig.dir.path().join("config.json"), cfg.to_string()).unwrap();
    let fresh = Arc::new(FakeHost::with(&[("PATH", "/usr/bin")]));
    rig.program(&fresh).apply();
    assert_eq!(fresh.get("PATH").as_deref(), Some("/usr/bin"));
    assert_eq!(fresh.get("FOO").as_deref(), Some("x"));
}

#[test]
fn refused_names_are_refused_for_each_class_and_nothing_is_written() {
    let rig = Rig::new();
    let host = Arc::new(FakeHost::default());
    let s = rig.program(&host);
    for (name, class) in [
        ("PATH", "system variable"),
        ("home", "system variable"),
        ("LD_PRELOAD", "load or run"),
        ("DYLD_INSERT_LIBRARIES", "load or run"),
        ("NODE_OPTIONS", "load or run"),
        ("GIT_SSH_COMMAND", "load or run"),
        ("RETASKABLE_SECRETS_KEY", "namespace"),
        ("RETASKABLE_LANE", "namespace"),
        ("ENABLE_CLAUDEAI_MCP_SERVERS", "sets itself"),
        ("CF_API_TOKEN", "sets itself"),
    ] {
        let why = refused(&s, &[(name, json!("x"))]);
        assert!(why.starts_with(name) && why.contains(class), "{name}: {why}");
    }
    assert_eq!(rig.text(), "");
    assert!(rig.os.names().is_empty(), "no key is made for a refused save");
    assert!(host.all().is_empty());
}

#[test]
fn bad_names_values_duplicates_and_too_many_are_refused() {
    let rig = Rig::new();
    let s = rig.program(&Arc::new(FakeHost::default()));
    assert!(refused(&s, &[("", json!("x"))]).contains("required"));
    assert!(refused(&s, &[("1ST", json!("x"))]).contains("letters, digits"));
    assert!(refused(&s, &[("A-B", json!("x"))]).contains("letters, digits"));
    assert!(refused(&s, &[(&"A".repeat(129), json!("x"))]).contains("128"));
    save(&s, &[(&"A".repeat(128), json!("x"))]);
    assert!(refused(&s, &[("FOO", json!(""))]).contains("needs a value"));
    assert!(refused(&s, &[("FOO", json!("a\0b"))]).contains("NUL"));
    assert!(refused(&s, &[("FOO", json!("a\nb"))]).contains("line break"));
    assert!(refused(&s, &[("FOO", json!("x".repeat(names::MAX_VALUE_LEN + 1)))]).contains("at most"));
    assert!(refused(&s, &[("FOO", json!(5))]).contains("must be text"));
    // The same name twice (case doesn't tell them apart), against a known secret, and against a saved one.
    assert!(refused(&s, &[("Foo", json!("x")), ("FOO", json!("y"))]).contains("twice"));
    assert!(refused(&s, &[("gh_token", json!("x"))]).contains("same variable as GH_TOKEN"));
    save(&s, &[("BAR", json!("x"))]);
    assert!(refused(&s, &[("bar", json!("y"))]).contains("twice"));
    // A custom variable's value may hold spaces; a known secret's may not.
    assert!(refused(&s, &[("GH_TOKEN", json!("a b"))]).contains("spaces"));
    let many: Vec<(String, Value)> = (0..names::MAX_CUSTOM + 1)
        .map(|i| (format!("V{i}"), json!("x")))
        .collect();
    let many: Vec<(&str, Value)> = many.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
    assert!(refused(&s, &many).contains("At most"));
}

#[test]
fn a_mixed_save_with_one_bad_variable_writes_none_of_it() {
    let rig = Rig::new();
    let host = Arc::new(FakeHost::default());
    let s = rig.program(&host);
    let why = refused(
        &s,
        &[("GOOD", json!("x")), ("PATH", json!("y")), ("GH_TOKEN", json!("tok"))],
    );
    assert!(why.starts_with("PATH"), "{why}");
    assert_eq!(rig.text(), "");
    assert!(host.all().is_empty());
}

#[test]
fn the_ci_runner_takes_custom_variables_in_its_own_file_and_applies_them_live() {
    let rig = Rig::new();
    let host = Arc::new(FakeHost::default());
    let ci = Secrets::build(
        Owner::CiRunner,
        rig.dir.path(),
        &Env::empty(),
        host.clone(),
        Some(rig.os.clone()),
        None,
    );
    ci.apply();
    let page = Secrets::for_page(
        Owner::CiRunner,
        rig.dir.path(),
        &Env::empty(),
        Some(rig.os.clone()),
        &[],
    );
    save(&page, &[("REGISTRY_TOKEN", json!(VALUE))]);
    assert_eq!(
        host.get("REGISTRY_TOKEN"),
        None,
        "the page never changes its own environment for a program"
    );
    ci.refresh();
    assert_eq!(host.get("REGISTRY_TOKEN").as_deref(), Some(VALUE));
    assert!(rig.text().is_empty(), "not in the code agent's config");
}
