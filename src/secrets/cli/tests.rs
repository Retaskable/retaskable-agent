use std::sync::Arc;

use anyhow::anyhow;

use super::*;
use crate::secrets::fakes::MemStore;

const TOKEN: &str = "sk-ant-oat01-PLANTED_claude_token_123456";

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_string()).collect()
}

fn secrets(root: &Path, env: &Env, os: Option<Arc<MemStore>>) -> Secrets {
    Secrets::for_page(
        Owner::CodeAgent,
        root,
        env,
        os.map(|o| o as Arc<dyn crate::link::TokenStore>),
        &[],
    )
}

fn never_asked(_: &str) -> Result<String> {
    Err(anyhow!("asked for a value"))
}

#[test]
fn set_saves_the_value_sealed_under_a_key_file_made_on_first_use_and_list_never_shows_it() {
    let root = tempfile::tempdir().unwrap();
    let key = root.path().join("secrets.key");
    let env = Env::from_pairs([(super::super::KEY_FILE_VAR, key.to_str().unwrap())]);
    let s = secrets(root.path(), &env, None);
    let out = run_with(
        &s,
        Owner::CodeAgent,
        &args(&["set", "CLAUDE_CODE_OAUTH_TOKEN"]),
        |name| {
            assert_eq!(name, "CLAUDE_CODE_OAUTH_TOKEN");
            Ok(format!("{TOKEN}\n"))
        },
    )
    .unwrap();
    assert!(out.contains(&fingerprint(TOKEN)) && !out.contains(TOKEN));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&key).unwrap().permissions().mode() & 0o777, 0o600);
    }
    let config = std::fs::read_to_string(root.path().join("config.json")).unwrap();
    assert!(config.contains("v1:") && !config.contains(TOKEN));
    // A fresh handle (the next process) reads it back with the same key.
    let again = secrets(root.path(), &env, None);
    let listed = run_with(&again, Owner::CodeAgent, &args(&["list"]), never_asked).unwrap();
    assert!(listed.contains(&format!("CLAUDE_CODE_OAUTH_TOKEN  saved  {}", fingerprint(TOKEN))));
    assert!(listed.contains("GH_TOKEN  unset"));
    assert!(!listed.contains(TOKEN));
}

#[test]
fn a_name_the_program_doesnt_take_is_refused_before_a_value_is_asked_for() {
    let root = tempfile::tempdir().unwrap();
    let s = secrets(root.path(), &Env::empty(), Some(Arc::new(MemStore::default())));
    let err = run_with(
        &s,
        Owner::CiRunner,
        &args(&["set", "CLAUDE_CODE_OAUTH_TOKEN"]),
        never_asked,
    )
    .unwrap_err();
    assert!(err.to_string().contains("GH_TOKEN"), "{err}");
    let err = run_with(&s, Owner::CodeAgent, &args(&["set", "NOPE"]), never_asked).unwrap_err();
    assert!(err.to_string().contains("isn't a secret"), "{err}");
}

#[test]
fn a_value_on_the_command_line_is_refused_and_so_is_an_empty_one() {
    let root = tempfile::tempdir().unwrap();
    let s = secrets(root.path(), &Env::empty(), Some(Arc::new(MemStore::default())));
    let err = run_with(&s, Owner::CodeAgent, &args(&["set", "GH_TOKEN", TOKEN]), never_asked).unwrap_err();
    assert!(err.to_string().contains("never an argument") && !err.to_string().contains(TOKEN));
    let err = run_with(&s, Owner::CodeAgent, &args(&["set", "GH_TOKEN"]), |_| Ok("  \n".into())).unwrap_err();
    assert!(err.to_string().contains("nothing was saved"));
    assert!(!root.path().join("config.json").exists());
    assert!(run_with(&s, Owner::CodeAgent, &args(&[]), never_asked).is_err());
}

#[test]
fn with_no_key_anywhere_nothing_is_saved_and_no_value_is_asked_for() {
    let root = tempfile::tempdir().unwrap();
    let s = secrets(root.path(), &Env::empty(), None);
    let err = run_with(&s, Owner::CodeAgent, &args(&["set", "GH_TOKEN"]), never_asked).unwrap_err();
    assert!(err.to_string().contains("RETASKABLE_SECRETS_KEY_FILE"), "{err}");
    assert!(!root.path().join("config.json").exists());
}

#[test]
fn a_key_file_others_can_read_is_refused() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let key = root.path().join("secrets.key");
        std::fs::write(&key, "x").unwrap();
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
        let env = Env::from_pairs([(super::super::KEY_FILE_VAR, key.to_str().unwrap())]);
        let s = secrets(root.path(), &env, None);
        let err = run_with(&s, Owner::CodeAgent, &args(&["set", "GH_TOKEN"]), never_asked).unwrap_err();
        assert!(err.to_string().contains("chmod 600"), "{err}");
    }
}

#[test]
fn the_ci_runner_saves_its_own_name_in_its_own_file() {
    let root = tempfile::tempdir().unwrap();
    let s = Secrets::for_page(
        Owner::CiRunner,
        root.path(),
        &Env::empty(),
        Some(Arc::new(MemStore::default())),
        &[],
    );
    run_with(&s, Owner::CiRunner, &args(&["set", "GH_TOKEN"]), |_| {
        Ok("ghp_PLANTED_ci_0123456789".into())
    })
    .unwrap();
    assert!(Owner::CiRunner.file(root.path()).exists());
    assert!(!root.path().join("config.json").exists());
}

#[test]
fn set_is_allowed_with_the_environment_set_and_reset_deletes_it_and_lists_the_environment_again() {
    let root = tempfile::tempdir().unwrap();
    let os = Arc::new(MemStore::default());
    let s = Secrets::for_page(
        Owner::CiRunner,
        root.path(),
        &Env::empty(),
        Some(os.clone() as Arc<dyn crate::link::TokenStore>),
        &["GH_TOKEN".to_string()],
    );
    let listed = run_with(&s, Owner::CiRunner, &args(&["list"]), never_asked).unwrap();
    assert!(listed.starts_with("GH_TOKEN  env"), "{listed}");
    run_with(&s, Owner::CiRunner, &args(&["set", "GH_TOKEN"]), |_| {
        Ok(format!("{TOKEN}\n"))
    })
    .unwrap();
    let listed = run_with(&s, Owner::CiRunner, &args(&["list"]), never_asked).unwrap();
    assert!(
        listed.contains(&format!("GH_TOKEN  saved  {}", fingerprint(TOKEN))),
        "{listed}"
    );
    let out = run_with(&s, Owner::CiRunner, &args(&["reset", "GH_TOKEN"]), never_asked).unwrap();
    assert!(out.contains("Deleted") && !out.contains(TOKEN));
    let listed = run_with(&s, Owner::CiRunner, &args(&["list"]), never_asked).unwrap();
    assert!(listed.starts_with("GH_TOKEN  env"), "{listed}");
    assert!(run_with(&s, Owner::CiRunner, &args(&["reset", "NOPE"]), never_asked).is_err());
}
