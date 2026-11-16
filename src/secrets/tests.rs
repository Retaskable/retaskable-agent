use std::io::Write;
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value, json};

use super::fakes::{FakeHost, MemStore};
use super::*;
use crate::env::Env;
use crate::link::FileStore;

const TOKEN: &str = "ghp_PLANTED_secret_value_1234567890";

struct Rig {
    dir: tempfile::TempDir,
    host: Arc<FakeHost>,
    os: Arc<MemStore>,
    legacy: Arc<MemStore>,
}

impl Rig {
    fn new(env: &[(&str, &str)]) -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
            host: Arc::new(FakeHost::with(env)),
            os: Arc::new(MemStore::default()),
            legacy: Arc::new(MemStore::default()),
        }
    }

    /// A `Secrets` over this rig's data folder, key variables from `key_env`, with an OS store.
    fn secrets(&self, key_env: &[(&str, &str)]) -> Secrets {
        Secrets::new(
            self.dir.path(),
            &Env::from_pairs(key_env.iter().copied()),
            self.host.clone(),
            Some(self.os.clone()),
            self.legacy.clone(),
        )
    }

    fn config_text(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("config.json")).unwrap_or_default()
    }

    fn stored(&self) -> Map<String, Value> {
        let v: Value = serde_json::from_str(&self.config_text()).unwrap();
        v["secrets"].as_object().cloned().unwrap_or_default()
    }
}

fn changes(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect()
}

fn save(secrets: &Secrets, pairs: &[(&str, Value)]) {
    let plan = secrets.plan(&changes(pairs)).unwrap();
    secrets.commit(plan).unwrap();
}

fn state(secrets: &Secrets, name: &str) -> (Source, Option<String>) {
    let s = secrets.states().into_iter().find(|s| s.name == name).unwrap();
    (s.source, s.fingerprint)
}

fn key_b64() -> String {
    DataKey::generate().to_base64().to_string()
}

// --- the cipher ---

#[test]
fn a_secret_seals_and_opens_back() {
    let key = DataKey::generate();
    let stored = seal(&key, "GH_TOKEN", TOKEN);
    assert!(stored.starts_with("v1:"));
    assert!(!stored.contains(TOKEN));
    assert_eq!(open(&key, "GH_TOKEN", &stored).unwrap(), TOKEN);
}

#[test]
fn the_wrong_key_a_changed_ciphertext_or_another_name_does_not_open() {
    let key = DataKey::generate();
    let stored = seal(&key, "GH_TOKEN", TOKEN);
    assert_eq!(open(&DataKey::generate(), "GH_TOKEN", &stored), Err(Unreadable));
    // Another name's: a ciphertext copied under a different name.
    assert_eq!(open(&key, "GITLAB_TOKEN", &stored), Err(Unreadable));
    // One bit flipped, in the middle of the sealed text.
    let body = stored.strip_prefix("v1:").unwrap();
    let mut bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, body).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    let tampered = format!(
        "v1:{}",
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes)
    );
    assert_eq!(open(&key, "GH_TOKEN", &tampered), Err(Unreadable));
    // Not a format this build knows, not base64, too short.
    for junk in ["", "v1:", "v1:!!!", "v2:AAAA", TOKEN, "v1:AAAA"] {
        assert_eq!(open(&key, "GH_TOKEN", junk), Err(Unreadable), "{junk}");
    }
}

#[test]
fn every_seal_has_its_own_nonce() {
    let key = DataKey::generate();
    assert_ne!(seal(&key, "GH_TOKEN", TOKEN), seal(&key, "GH_TOKEN", TOKEN));
}

#[test]
fn a_key_is_32_bytes_of_base64_and_never_printed() {
    let key = DataKey::generate();
    assert_eq!(
        DataKey::from_base64(&format!(" {}\n", key.to_base64().as_str())),
        Some(key.clone())
    );
    assert_eq!(DataKey::from_base64("AAAA"), None);
    assert_eq!(DataKey::from_base64("not base64!"), None);
    assert!(!format!("{key:?}").contains(key.to_base64().as_str()));
}

#[test]
fn the_fingerprint_is_the_first_8_hex_of_the_sha256() {
    assert_eq!(fingerprint("abc"), "ba7816bf");
}

// --- saving, and where it lands ---

#[test]
fn a_saved_secret_is_ciphertext_in_the_config_and_opens_again_on_a_fresh_start() {
    let rig = Rig::new(&[]);
    let secrets = rig.secrets(&[]);
    save(&secrets, &[("GH_TOKEN", json!(TOKEN))]);
    assert!(!rig.config_text().contains(TOKEN), "plaintext reached config.json");
    assert!(rig.stored()["GH_TOKEN"].as_str().unwrap().starts_with("v1:"));
    assert_eq!(rig.host.get("GH_TOKEN").as_deref(), Some(TOKEN));

    // A new process: the environment is gone, the config and the OS key remain.
    let host = Arc::new(FakeHost::default());
    let again = Secrets::new(
        rig.dir.path(),
        &Env::empty(),
        host.clone(),
        Some(rig.os.clone()),
        rig.legacy.clone(),
    );
    again.apply();
    assert_eq!(host.get("GH_TOKEN").as_deref(), Some(TOKEN));
    assert_eq!(state(&again, "GH_TOKEN"), (Source::Saved, Some(fingerprint(TOKEN))));
}

#[test]
fn two_saves_of_the_same_value_differ_on_disk() {
    let rig = Rig::new(&[]);
    let secrets = rig.secrets(&[]);
    save(&secrets, &[("GH_TOKEN", json!(TOKEN))]);
    let first = rig.stored()["GH_TOKEN"].clone();
    save(&secrets, &[("GH_TOKEN", json!(TOKEN))]);
    assert_ne!(first, rig.stored()["GH_TOKEN"]);
}

#[test]
fn saving_keeps_every_other_key_in_the_config() {
    let rig = Rig::new(&[]);
    std::fs::write(
        rig.dir.path().join("config.json"),
        r#"{"version":2,"projects":[],"pollIntervalSec":30,"somethingNew":{"a":1}}"#,
    )
    .unwrap();
    save(&rig.secrets(&[]), &[("GH_TOKEN", json!(TOKEN))]);
    let v: Value = serde_json::from_str(&rig.config_text()).unwrap();
    assert_eq!(v["pollIntervalSec"], 30);
    assert_eq!(v["somethingNew"], json!({"a": 1}));
    assert!(v["secrets"]["GH_TOKEN"].is_string());
    // The settings page's own save of something else keeps the secrets.
    config::save_machine_settings(rig.dir.path(), |o| {
        o.insert("port".into(), json!(4800));
    })
    .unwrap();
    let v: Value = serde_json::from_str(&rig.config_text()).unwrap();
    assert!(v["secrets"]["GH_TOKEN"].is_string());
    assert!(config::load(rig.dir.path()).is_ok());
}

#[test]
fn clearing_removes_the_entry_and_the_variable() {
    let rig = Rig::new(&[]);
    let secrets = rig.secrets(&[]);
    save(
        &secrets,
        &[("GH_TOKEN", json!(TOKEN)), ("GITLAB_TOKEN", json!("glpat-x"))],
    );
    assert_eq!(
        rig.host.get("RETASKABLE_SAVED_SECRETS").as_deref(),
        Some("GH_TOKEN,GITLAB_TOKEN")
    );
    save(&secrets, &[("GH_TOKEN", Value::Null)]);
    assert_eq!(rig.host.get("GH_TOKEN"), None);
    assert_eq!(
        rig.host.get("RETASKABLE_SAVED_SECRETS").as_deref(),
        Some("GITLAB_TOKEN")
    );
    assert!(rig.stored().get("GH_TOKEN").is_none());
    save(&secrets, &[("GITLAB_TOKEN", Value::Null)]);
    assert!(!rig.config_text().contains("secrets"));
    assert_eq!(rig.host.get("RETASKABLE_SAVED_SECRETS"), None);
    assert_eq!(state(&secrets, "GH_TOKEN"), (Source::Unset, None));
}

#[test]
fn a_save_is_checked_whole_before_any_of_it_is_written() {
    let rig = Rig::new(&[]);
    let secrets = rig.secrets(&[]);
    let bad = |value: Value| secrets.plan(&changes(&[("GH_TOKEN", json!("ok-value")), ("GITLAB_TOKEN", value)]));
    for value in [
        json!(""),
        json!("a b"),
        json!("a\nb"),
        json!(5),
        json!("x".repeat(4097)),
    ] {
        assert!(bad(value).is_err());
    }
    assert!(secrets.plan(&changes(&[("PATH", json!("x"))])).is_err());
    assert_eq!(rig.config_text(), "");
    assert!(rig.os.names().is_empty(), "no key is made for a refused save");
    assert!(secrets.plan(&changes(&[("GH_TOKEN", json!("  padded  "))])).is_ok());
}

// --- a saved secret wins over the environment (decided 2026-11-16) ---

/// A rig whose environment already holds `GH_TOKEN=from-env` and whose config has `TOKEN` saved for it.
fn saved_and_env() -> (Rig, Secrets) {
    let rig = Rig::new(&[("GH_TOKEN", "from-env")]);
    let before = Secrets::new(
        rig.dir.path(),
        &Env::empty(),
        Arc::new(FakeHost::default()),
        Some(rig.os.clone()),
        rig.legacy.clone(),
    );
    save(&before, &[("GH_TOKEN", json!(TOKEN))]);
    let secrets = rig.secrets(&[]);
    secrets.apply();
    (rig, secrets)
}

#[test]
fn a_saved_secret_replaces_the_environments_value_in_the_process() {
    let (rig, secrets) = saved_and_env();
    assert_eq!(rig.host.get("GH_TOKEN").as_deref(), Some(TOKEN));
    assert_eq!(state(&secrets, "GH_TOKEN"), (Source::Saved, Some(fingerprint(TOKEN))));
    let row = secrets.states().into_iter().find(|s| s.name == "GH_TOKEN").unwrap();
    assert!(row.env_set, "the page can say what a reset goes back to");
    assert_eq!(secrets.env_names(), ["GH_TOKEN"]);
    assert_eq!(rig.host.get("RETASKABLE_SAVED_SECRETS").as_deref(), Some("GH_TOKEN"));
    // Nothing is locked: a new value and a delete are both accepted.
    assert!(secrets.plan(&changes(&[("GH_TOKEN", json!("new"))])).is_ok());
    assert!(secrets.plan(&changes(&[("GH_TOKEN", Value::Null)])).is_ok());
}

#[test]
fn the_environment_fills_a_secret_that_was_never_saved() {
    let rig = Rig::new(&[("GH_TOKEN", "from-env")]);
    let secrets = rig.secrets(&[]);
    secrets.apply();
    assert_eq!(rig.host.get("GH_TOKEN").as_deref(), Some("from-env"));
    assert_eq!(
        state(&secrets, "GH_TOKEN"),
        (Source::Env, Some(fingerprint("from-env")))
    );
    // It isn't recorded as one the agent put there, and saving over it is allowed.
    assert_eq!(rig.host.get("RETASKABLE_SAVED_SECRETS"), None);
    save(&secrets, &[("GH_TOKEN", json!(TOKEN))]);
    assert_eq!(rig.host.get("GH_TOKEN").as_deref(), Some(TOKEN));
    assert_eq!(state(&secrets, "GH_TOKEN").0, Source::Saved);
}

#[test]
fn deleting_a_saved_secret_goes_back_to_the_environment_it_started_with() {
    let (rig, secrets) = saved_and_env();
    let git_logins = rig.host.git_logins();
    save(&secrets, &[("GH_TOKEN", Value::Null)]);
    assert_eq!(rig.host.get("GH_TOKEN").as_deref(), Some("from-env"));
    assert_eq!(
        state(&secrets, "GH_TOKEN"),
        (Source::Env, Some(fingerprint("from-env")))
    );
    assert_eq!(rig.host.get("RETASKABLE_SAVED_SECRETS"), None);
    assert_eq!(
        rig.host.git_logins(),
        git_logins + 1,
        "git is pointed at the token now in use"
    );
    // A save after that replaces it again, and a later delete still has the original.
    save(&secrets, &[("GH_TOKEN", json!("second"))]);
    assert_eq!(rig.host.get("GH_TOKEN").as_deref(), Some("second"));
    save(&secrets, &[("GH_TOKEN", Value::Null)]);
    assert_eq!(rig.host.get("GH_TOKEN").as_deref(), Some("from-env"));
}

#[test]
fn a_refresh_applies_a_change_made_by_another_process_over_the_environment() {
    let rig = Rig::new(&[("GH_TOKEN", "from-env")]);
    let running = rig.secrets(&[]);
    running.apply();
    // The page (another process) saves it, then deletes it.
    let other = Secrets::for_page(
        Owner::CodeAgent,
        rig.dir.path(),
        &Env::empty(),
        Some(rig.os.clone()),
        &[],
    );
    save(&other, &[("GH_TOKEN", json!(TOKEN))]);
    running.refresh();
    assert_eq!(rig.host.get("GH_TOKEN").as_deref(), Some(TOKEN));
    assert_eq!(state(&running, "GH_TOKEN").0, Source::Saved);
    save(&other, &[("GH_TOKEN", Value::Null)]);
    running.refresh();
    assert_eq!(rig.host.get("GH_TOKEN").as_deref(), Some("from-env"));
    assert_eq!(state(&running, "GH_TOKEN").0, Source::Env);
}

#[test]
fn a_saved_secret_that_wont_open_fails_closed_and_never_falls_back_to_the_environment() {
    let (rig, _) = saved_and_env();
    // A different computer's key: the saved one no longer opens.
    let wrong = Secrets::new(
        rig.dir.path(),
        &Env::from_pairs([(KEY_VAR, key_b64().as_str())]),
        rig.host.clone(),
        None,
        rig.legacy.clone(),
    );
    rig.host.set("GH_TOKEN", "from-env");
    wrong.apply();
    assert_eq!(
        rig.host.get("GH_TOKEN"),
        None,
        "not the environment's value, not the saved one"
    );
    assert_eq!(state(&wrong, "GH_TOKEN"), (Source::Unreadable, None));
    // Deleting it is how the owner gets back to the environment's value.
    save(&wrong, &[("GH_TOKEN", Value::Null)]);
    assert_eq!(rig.host.get("GH_TOKEN").as_deref(), Some("from-env"));
}

#[test]
fn an_empty_variable_counts_as_unset() {
    let rig = Rig::new(&[("GH_TOKEN", ""), ("GITLAB_TOKEN", "   ")]);
    let before = Secrets::new(
        rig.dir.path(),
        &Env::empty(),
        Arc::new(FakeHost::default()),
        Some(rig.os.clone()),
        rig.legacy.clone(),
    );
    save(&before, &[("GH_TOKEN", json!(TOKEN))]);
    let secrets = rig.secrets(&[]);
    secrets.apply();
    assert_eq!(rig.host.get("GH_TOKEN").as_deref(), Some(TOKEN));
    assert_eq!(state(&secrets, "GH_TOKEN").0, Source::Saved);
    assert_eq!(state(&secrets, "GITLAB_TOKEN"), (Source::Unset, None));
    assert!(secrets.plan(&changes(&[("GITLAB_TOKEN", json!("glpat"))])).is_ok());
}

#[test]
fn a_restart_still_tells_the_saved_ones_from_the_environments() {
    let rig = Rig::new(&[]);
    save(
        &rig.secrets(&[]),
        &[("GH_TOKEN", json!(TOKEN)), ("GITLAB_TOKEN", json!("glpat-1"))],
    );
    // The restarted agent inherits the first one's environment: both saved values and the names.
    let host = Arc::new(FakeHost::with(&[
        ("GH_TOKEN", TOKEN),
        ("GITLAB_TOKEN", "someone-changed-it"),
        ("RETASKABLE_SAVED_SECRETS", "GH_TOKEN,GITLAB_TOKEN"),
    ]));
    let again = Secrets::new(
        rig.dir.path(),
        &Env::empty(),
        host.clone(),
        Some(rig.os.clone()),
        rig.legacy.clone(),
    );
    again.apply();
    assert_eq!(state(&again, "GH_TOKEN").0, Source::Saved);
    // One the environment has since changed is still beaten by the saved one, which the environment's value was
    // not (so it is the one a delete goes back to).
    assert_eq!(state(&again, "GITLAB_TOKEN").0, Source::Saved);
    assert_eq!(host.get("GITLAB_TOKEN").as_deref(), Some("glpat-1"));
    assert_eq!(
        host.get("RETASKABLE_SAVED_SECRETS").as_deref(),
        Some("GH_TOKEN,GITLAB_TOKEN")
    );
    save(&again, &[("GITLAB_TOKEN", Value::Null)]);
    assert_eq!(host.get("GITLAB_TOKEN").as_deref(), Some("someone-changed-it"));
}

// --- apply ---

#[test]
fn apply_puts_saved_secrets_in_the_environment_and_names_them() {
    let rig = Rig::new(&[]);
    save(
        &rig.secrets(&[]),
        &[("GH_TOKEN", json!(TOKEN)), ("CLOUDFLARE_ACCOUNT_ID", json!("acct1"))],
    );
    let host = Arc::new(FakeHost::default());
    let fresh = Secrets::new(
        rig.dir.path(),
        &Env::empty(),
        host.clone(),
        Some(rig.os.clone()),
        rig.legacy.clone(),
    );
    fresh.apply();
    assert_eq!(host.get("GH_TOKEN").as_deref(), Some(TOKEN));
    assert_eq!(host.get("CLOUDFLARE_ACCOUNT_ID").as_deref(), Some("acct1"));
    assert_eq!(
        host.get("RETASKABLE_SAVED_SECRETS").as_deref(),
        Some("CLOUDFLARE_ACCOUNT_ID,GH_TOKEN")
    );
    assert_eq!(host.git_logins(), 1, "a saved GH_TOKEN runs gh auth setup-git at start");
}

#[test]
fn git_login_runs_for_a_changed_gh_token_and_for_no_other() {
    let rig = Rig::new(&[]);
    let secrets = rig.secrets(&[]);
    save(&secrets, &[("GITLAB_TOKEN", json!("glpat-1"))]);
    assert_eq!(rig.host.git_logins(), 0);
    save(&secrets, &[("GH_TOKEN", json!(TOKEN))]);
    assert_eq!(rig.host.git_logins(), 1);
    save(&secrets, &[("GH_TOKEN", Value::Null)]);
    assert_eq!(rig.host.git_logins(), 1);
}

// --- fail closed ---

#[test]
fn a_wrong_key_reads_as_unreadable_never_plaintext_and_never_a_crash() {
    let rig = Rig::new(&[]);
    save(
        &rig.secrets(&[("RETASKABLE_SECRETS_KEY", &key_b64())]),
        &[("GH_TOKEN", json!(TOKEN))],
    );
    // Another key, as after a restored config on another computer.
    let host = Arc::new(FakeHost::default());
    let other = Secrets::new(
        rig.dir.path(),
        &Env::from_pairs([("RETASKABLE_SECRETS_KEY", key_b64())]),
        host.clone(),
        None,
        rig.legacy.clone(),
    );
    other.apply();
    assert_eq!(state(&other, "GH_TOKEN"), (Source::Unreadable, None));
    assert_eq!(host.get("GH_TOKEN"), None);
    assert!(!format!("{:?}", host.all()).contains(TOKEN));
    // Saving it again makes it readable under this key.
    save(&other, &[("GH_TOKEN", json!(TOKEN))]);
    assert_eq!(state(&other, "GH_TOKEN").0, Source::Saved);
}

#[test]
fn a_missing_key_and_a_ciphertext_moved_to_another_name_are_unreadable() {
    let rig = Rig::new(&[]);
    save(&rig.secrets(&[]), &[("GH_TOKEN", json!(TOKEN))]);
    // The OS key is gone.
    rig.os.delete(&account_for(rig.dir.path())).unwrap();
    let lost = rig.secrets(&[]);
    assert_eq!(state(&lost, "GH_TOKEN").0, Source::Unreadable);

    // A ciphertext swapped under another name in the config.
    let rig = Rig::new(&[]);
    save(&rig.secrets(&[]), &[("GH_TOKEN", json!(TOKEN))]);
    let sealed = rig.stored()["GH_TOKEN"].clone();
    config::save_machine_settings(rig.dir.path(), |o| {
        o["secrets"]["GITLAB_TOKEN"] = sealed;
    })
    .unwrap();
    let secrets = rig.secrets(&[]);
    assert_eq!(state(&secrets, "GITLAB_TOKEN").0, Source::Unreadable);
    assert_eq!(state(&secrets, "GH_TOKEN").0, Source::Saved);
}

#[test]
fn a_locked_os_store_reads_as_unreadable() {
    let rig = Rig::new(&[]);
    save(&rig.secrets(&[]), &[("GH_TOKEN", json!(TOKEN))]);
    *rig.os.broken.lock().unwrap() = true;
    assert_eq!(state(&rig.secrets(&[]), "GH_TOKEN").0, Source::Unreadable);
}

// --- where the key comes from ---

#[test]
fn the_os_store_keeps_one_key_per_data_folder_made_on_the_first_save() {
    let rig = Rig::new(&[]);
    let secrets = rig.secrets(&[]);
    assert!(rig.os.names().is_empty(), "starting makes no key");
    save(&secrets, &[("GH_TOKEN", json!(TOKEN))]);
    assert_eq!(rig.os.names(), vec![account_for(rig.dir.path())]);
    let other = tempfile::tempdir().unwrap();
    assert_ne!(account_for(rig.dir.path()), account_for(other.path()));
    assert_eq!(account_for(rig.dir.path()).len(), 32);
    // The config holds ciphertext and not the key.
    let key = rig.os.get(&account_for(rig.dir.path())).unwrap().unwrap();
    assert!(!rig.config_text().contains(&key));
}

#[test]
fn a_headless_key_comes_from_the_environment_variable() {
    let rig = Rig::new(&[("RETASKABLE_SECRETS_KEY", "")]);
    let key = key_b64();
    let host = Arc::new(FakeHost::with(&[("RETASKABLE_SECRETS_KEY", &key)]));
    let env = Env::from_pairs([("RETASKABLE_SECRETS_KEY", key.clone())]);
    let secrets = Secrets::new(rig.dir.path(), &env, host.clone(), None, rig.legacy.clone());
    assert_eq!(secrets.save_problem(), None);
    secrets.apply();
    // Read once; no child inherits it.
    assert_eq!(host.get("RETASKABLE_SECRETS_KEY"), None);
    save(&secrets, &[("GH_TOKEN", json!(TOKEN))]);
    let again = Secrets::new(
        rig.dir.path(),
        &env,
        Arc::new(FakeHost::default()),
        None,
        rig.legacy.clone(),
    );
    assert_eq!(state(&again, "GH_TOKEN").0, Source::Saved);
    assert!(!rig.config_text().contains(&key));
}

#[test]
fn a_malformed_key_variable_says_so_and_nothing_saves() {
    let rig = Rig::new(&[]);
    let secrets = rig.secrets(&[("RETASKABLE_SECRETS_KEY", "short")]);
    assert!(secrets.save_problem().unwrap().contains("RETASKABLE_SECRETS_KEY"));
    assert!(secrets.plan(&changes(&[("GH_TOKEN", json!("x"))])).is_err());
}

#[test]
fn a_headless_key_comes_from_a_file_and_the_first_save_makes_it() {
    let rig = Rig::new(&[]);
    let file = rig.dir.path().join("keys").join("secrets.key");
    let env = [("RETASKABLE_SECRETS_KEY_FILE", file.to_str().unwrap())];
    let secrets = rig.secrets(&env);
    assert_eq!(secrets.save_problem(), None);
    assert!(!file.exists(), "starting makes no file");
    save(&secrets, &[("GH_TOKEN", json!(TOKEN))]);
    assert!(DataKey::from_base64(&std::fs::read_to_string(&file).unwrap()).is_some());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
    }
    // The file wins over the OS store, which was never asked.
    assert!(rig.os.names().is_empty());
    assert_eq!(state(&rig.secrets(&env), "GH_TOKEN").0, Source::Saved);
}

#[cfg(unix)]
#[test]
fn a_key_file_others_can_read_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let rig = Rig::new(&[]);
    let file = rig.dir.path().join("secrets.key");
    std::fs::File::create(&file)
        .unwrap()
        .write_all(key_b64().as_bytes())
        .unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    let secrets = rig.secrets(&[("RETASKABLE_SECRETS_KEY_FILE", file.to_str().unwrap())]);
    assert!(secrets.save_problem().unwrap().contains("chmod 600"));
    assert!(secrets.plan(&changes(&[("GH_TOKEN", json!("x"))])).is_err());
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        rig.secrets(&[("RETASKABLE_SECRETS_KEY_FILE", file.to_str().unwrap())])
            .save_problem(),
        None
    );
}

#[test]
fn a_key_file_that_is_not_a_key_is_refused() {
    let rig = Rig::new(&[]);
    let file = rig.dir.path().join("secrets.key");
    std::fs::write(&file, "nope").unwrap();
    #[cfg(unix)]
    std::fs::set_permissions(&file, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
    let secrets = rig.secrets(&[("RETASKABLE_SECRETS_KEY_FILE", file.to_str().unwrap())]);
    assert!(secrets.save_problem().unwrap().contains("32-byte"));
}

#[test]
fn with_no_os_store_and_no_key_secrets_cannot_be_saved_and_the_note_says_so() {
    let rig = Rig::new(&[]);
    let secrets = Secrets::new(
        rig.dir.path(),
        &Env::empty(),
        rig.host.clone(),
        None,
        rig.legacy.clone(),
    );
    let note = secrets.save_problem().unwrap();
    assert!(note.contains("RETASKABLE_SECRETS_KEY_FILE") && note.contains("RETASKABLE_SECRETS_KEY"));
    assert_eq!(
        secrets.plan(&changes(&[("GH_TOKEN", json!("x"))])).err().unwrap().0,
        note
    );
    assert_eq!(rig.config_text(), "");
    // Clearing needs no key.
    assert!(secrets.plan(&changes(&[("GH_TOKEN", Value::Null)])).is_ok());
    secrets.apply();
    assert_eq!(state(&secrets, "GH_TOKEN"), (Source::Unset, None));
}

// --- the move from the TypeScript agent ---

#[test]
fn the_typescript_agents_credential_entries_move_into_the_config_and_are_deleted() {
    let rig = Rig::new(&[]);
    let legacy = Arc::new(MemStore::with(&[("GH_TOKEN", TOKEN), ("GITLAB_TOKEN", "glpat-old")]));
    let secrets = Secrets::new(
        rig.dir.path(),
        &Env::empty(),
        rig.host.clone(),
        Some(rig.os.clone()),
        legacy.clone(),
    );
    secrets.apply();
    assert!(legacy.names().is_empty(), "the old entries are deleted");
    assert_eq!(rig.host.get("GH_TOKEN").as_deref(), Some(TOKEN));
    assert_eq!(state(&secrets, "GITLAB_TOKEN").0, Source::Saved);
    assert!(!rig.config_text().contains(TOKEN));
    assert!(rig.stored()["GH_TOKEN"].as_str().unwrap().starts_with("v1:"));
    // Again: nothing left to move, and what is there isn't touched.
    let sealed = rig.stored();
    secrets.apply();
    assert_eq!(rig.stored(), sealed);
}

#[test]
fn the_typescript_agents_credentials_file_moves_too_and_keeps_the_link_tokens() {
    let rig = Rig::new(&[]);
    let file = rig.dir.path().join("credentials.json");
    FileStore::new(file.clone(), "retaskable-secret")
        .set("GH_TOKEN", TOKEN)
        .unwrap();
    FileStore::new(file.clone(), "retaskable")
        .set("https://retaskable.com", "refresh")
        .unwrap();
    let legacy: Arc<dyn TokenStore> = Arc::new(FileStore::new(file.clone(), "retaskable-secret"));
    let secrets = Secrets::new(
        rig.dir.path(),
        &Env::empty(),
        rig.host.clone(),
        Some(rig.os.clone()),
        legacy,
    );
    secrets.apply();
    assert_eq!(rig.host.get("GH_TOKEN").as_deref(), Some(TOKEN));
    let left = std::fs::read_to_string(&file).unwrap();
    assert!(!left.contains(TOKEN));
    assert!(left.contains("refresh"), "the link token stays");
}

#[test]
fn an_existing_entry_in_the_config_wins_over_an_old_one() {
    let rig = Rig::new(&[]);
    save(&rig.secrets(&[]), &[("GH_TOKEN", json!("the-new-one"))]);
    let legacy = Arc::new(MemStore::with(&[("GH_TOKEN", "the-old-one")]));
    let secrets = Secrets::new(
        rig.dir.path(),
        &Env::empty(),
        rig.host.clone(),
        Some(rig.os.clone()),
        legacy.clone(),
    );
    secrets.apply();
    assert_eq!(rig.host.get("GH_TOKEN").as_deref(), Some("the-new-one"));
    assert_eq!(legacy.names(), vec!["GH_TOKEN".to_string()], "left alone");
}

#[test]
fn with_no_key_to_seal_with_an_old_entry_stays_where_it_is() {
    let rig = Rig::new(&[]);
    let legacy = Arc::new(MemStore::with(&[("GH_TOKEN", TOKEN)]));
    let secrets = Secrets::new(rig.dir.path(), &Env::empty(), rig.host.clone(), None, legacy.clone());
    secrets.apply();
    assert_eq!(legacy.names(), vec!["GH_TOKEN".to_string()]);
    assert_eq!(rig.config_text(), "");
    assert_eq!(rig.host.get("GH_TOKEN"), None);
}

#[test]
fn an_unreadable_old_store_is_skipped_not_fatal() {
    let rig = Rig::new(&[]);
    let legacy = Arc::new(MemStore::with(&[("GH_TOKEN", TOKEN)]));
    *legacy.broken.lock().unwrap() = true;
    let secrets = Secrets::new(
        rig.dir.path(),
        &Env::empty(),
        rig.host.clone(),
        Some(rig.os.clone()),
        legacy,
    );
    secrets.apply();
    assert_eq!(state(&secrets, "GH_TOKEN").0, Source::Unset);
}

// --- never logged ---

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
    type Writer = Captured;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[test]
fn no_value_and_no_key_reaches_a_log_line() {
    let log = Captured::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(log.clone())
        .with_max_level(tracing::Level::TRACE)
        .finish();
    let key = key_b64();
    tracing::subscriber::with_default(subscriber, || {
        // Another test's thread, with no subscriber, may have cached these call sites as off.
        tracing::callsite::rebuild_interest_cache();
        let rig = Rig::new(&[("CLOUDFLARE_API_TOKEN", "env-PLANTED-value")]);
        // Moving, saving, clearing, a wrong key, an old store that fails, a key problem.
        let legacy = Arc::new(MemStore::with(&[("GITLAB_TOKEN", "legacy-PLANTED-value")]));
        let env = Env::from_pairs([("RETASKABLE_SECRETS_KEY", key.clone())]);
        let secrets = Secrets::new(rig.dir.path(), &env, rig.host.clone(), None, legacy);
        secrets.apply();
        save(&secrets, &[("GH_TOKEN", json!(TOKEN))]);
        save(&secrets, &[("GH_TOKEN", Value::Null)]);
        save(&secrets, &[("GH_TOKEN", json!(TOKEN))]);
        let other = Env::from_pairs([("RETASKABLE_SECRETS_KEY", key_b64())]);
        Secrets::new(rig.dir.path(), &other, rig.host.clone(), None, rig.legacy.clone()).apply();
        let _ = secrets.plan(&changes(&[("GH_TOKEN", json!("bad value"))]));
        let _ = secrets.plan(&changes(&[("CLOUDFLARE_API_TOKEN", json!("x"))]));
        Secrets::new(
            rig.dir.path(),
            &Env::empty(),
            rig.host.clone(),
            None,
            rig.legacy.clone(),
        )
        .apply();
    });
    let text = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
    assert!(!text.is_empty(), "the capture saw the log lines");
    for planted in [
        TOKEN,
        "env-PLANTED-value",
        "legacy-PLANTED-value",
        key.as_str(),
        "bad value",
    ] {
        assert!(!text.contains(planted), "{planted} reached the log:\n{text}");
    }
}
