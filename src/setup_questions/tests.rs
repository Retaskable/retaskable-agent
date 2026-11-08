use std::io::Cursor;

use serde_json::Value;

use super::*;
use crate::password;
use crate::tags;

struct Run {
    out: String,
    dir: tempfile::TempDir,
}

fn run_with(dir: tempfile::TempDir, env: &Env, is_terminal: bool, input: &str) -> Run {
    let mut reader = Cursor::new(input.to_string());
    let mut out = Vec::new();
    let mut secret = |r: &mut dyn BufRead| {
        let mut line = String::new();
        r.read_line(&mut line)?;
        Ok(line)
    };
    let mut term = Terminal {
        is_terminal,
        input: &mut reader,
        out: &mut out,
        secret: &mut secret,
    };
    run(dir.path(), env, 4700, &mut term).unwrap();
    Run {
        out: String::from_utf8(out).unwrap(),
        dir,
    }
}

fn fresh(env: &Env, input: &str) -> Run {
    run_with(tempfile::tempdir().unwrap(), env, true, input)
}

fn files(run: &Run) -> (String, String) {
    let read = |name: &str| std::fs::read_to_string(run.dir.path().join(name)).unwrap_or_default();
    (read("config.json"), read("machine.json"))
}

#[test]
fn typed_answers_are_saved_to_config_and_machine_json() {
    // port, local (on, model, address), poll, tags, three Cursor tiers, network on + password.
    let input =
        "4800\non\nllama3\nhttp://localhost:1234\n30\nalpha, beta\ntop-model\nauto\nscout-model\non\nhunter2hunter2\n";
    let run = fresh(&Env::empty(), input);
    let (config, machine) = files(&run);
    assert!(config.contains("4800"), "{config}");
    assert!(
        config.contains("llama3") && config.contains("http://localhost:1234"),
        "{config}"
    );
    assert!(config.contains("30"), "{config}");
    assert!(
        config.contains("top-model") && config.contains("scout-model"),
        "{config}"
    );
    assert!(!config.contains("\"implementer\": \"auto\""), "{config}");
    assert!(machine.contains("alpha") && machine.contains("beta"), "{machine}");
    assert!(machine.contains("scrypt$"), "{machine}");
    assert!(
        run.out
            .contains("Saved. If Retaskable is running, restart it to use them.")
    );
}

#[test]
fn enter_on_every_question_changes_nothing() {
    let run = fresh(&Env::empty(), "\n\n\n\n\n\n\n");
    assert_eq!(files(&run), (String::new(), String::new()));
    assert!(!run.out.contains("Saved."));
    assert!(run.out.contains("Port [4700]: "));
}

#[test]
fn a_setting_a_variable_sets_is_said_not_asked() {
    let env = Env::from_pairs([("RETASKABLE_PORT", "4900"), ("RETASKABLE_POLL_SEC", "10")]);
    let run = fresh(&env, "\n\n\n\n\n\n");
    assert!(run.out.contains("Port: set by RETASKABLE_PORT, change it there."));
    assert!(run.out.contains("set by RETASKABLE_POLL_SEC"));
    assert!(!run.out.contains("Port ["));
    assert!(!run.out.contains("How often to check for work, in seconds ["));
}

#[test]
fn an_invalid_port_is_refused_in_the_pages_words_and_asked_again() {
    let run = fresh(&Env::empty(), "80\nabc\n4801\n");
    assert_eq!(run.out.matches("Port must be 1024 to 65535.").count(), 2);
    assert!(files(&run).0.contains("4801"));
}

#[test]
fn network_access_with_a_short_password_asks_again_and_hashes_with_scrypt() {
    let run = fresh(&Env::empty(), "\n\n\n\n\n\n\non\nshort\nlong-enough-1\n");
    assert!(run.out.contains("Password must be at least 8 characters."));
    let (_, machine) = files(&run);
    let hash = serde_json::from_str::<Value>(&machine).unwrap()["network"]["passwordHash"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(hash.starts_with("scrypt$"));
    assert!(password::verify_password("long-enough-1", &hash));
}

#[test]
fn network_access_on_with_no_password_stays_off() {
    let run = fresh(&Env::empty(), "\n\n\n\n\n\n\non\n\n");
    assert!(run.out.contains("stays off"));
    let machine = files(&run).1;
    assert!(!machine.contains("\"enabled\": true"), "{machine}");
}

#[test]
fn without_a_terminal_nothing_is_asked() {
    let run = run_with(tempfile::tempdir().unwrap(), &Env::empty(), false, "4800\n");
    assert_eq!(run.out, "");
    assert_eq!(files(&run), (String::new(), String::new()));
}

#[test]
fn none_clears_the_tags() {
    let found = tags::found_tags(&tags::machine_facts(&Env::empty()));
    let run = fresh(&Env::empty(), "\n\n\nnone\n");
    let machine = files(&run).1;
    let saved: Value = serde_json::from_str(&machine).unwrap();
    assert_eq!(saved["tags"]["removed"].as_array().unwrap().len(), found.len());
}
