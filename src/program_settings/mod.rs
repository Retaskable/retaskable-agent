//! What each of the three programs keeps in its own `settings.json` (docs/specs/programs.md, agent-settings.md section
//! 3.4): the CI runner's in `<data folder>/ci`, the Chat Agent's in `<data folder>/chat`. The code agent's stays its
//! `config.json`. Each program has its own file and nothing is shared between them but the data folder, so any can be
//! installed and run alone and none reads another's file.
//!
//! Layering, the same for every field: the environment first, then the file, then the default. The settings page edits
//! the file only; a field the environment sets is shown, locked, and a save naming it is refused.
//!
//! A neutral module (the boundary test, `program_boundaries_tests.rs`): the program that owns a file and the page that
//! edits it both import this, and it imports neither. It holds no value of a secret; `secrets` in a file is ciphertext
//! that `crate::secrets` opens.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};

use crate::env::Env;

pub mod chat;
pub mod ci;

/// The file's name inside a program's folder.
pub const FILE: &str = "settings.json";

/// The settings file of the program whose folder is `folder`.
pub fn file_in(folder: &Path) -> PathBuf {
    folder.join(FILE)
}

/// A save the page's caller got wrong, in words for the page. Never a value a person typed.
#[derive(Debug, PartialEq, Eq)]
pub struct Refused(pub String);

/// One setting: its name in the file and page, and the environment variable that overrides it.
pub struct Field {
    pub name: &'static str,
    pub env: &'static str,
}

/// The names of the `fields` the environment sets (an empty variable is none): the page locks these.
pub fn locked(env: &Env, fields: &[Field]) -> Vec<&'static str> {
    fields
        .iter()
        .filter(|f| env.get(f.env).is_some_and(|v| !v.trim().is_empty()))
        .map(|f| f.name)
        .collect()
}

/// The file as a JSON object: no file is an empty one. A file that isn't a JSON object is an error that names the
/// file, so a reader keeps what it had and a writer never overwrites what a person may have been editing.
pub fn read_object(file: &Path) -> Result<Map<String, Value>> {
    let text = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", file.display())),
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => bail!("{} must hold a JSON object.", file.display()),
        Err(e) => bail!("{} isn't valid JSON: {e}", file.display()),
    }
}

/// Changes the file whole or not at all (a temp file, then a rename), keeping every key `patch` leaves alone: a key a
/// newer version wrote survives an older one's save.
pub fn patch(file: &Path, change: impl FnOnce(&mut Map<String, Value>)) -> Result<()> {
    let mut obj = read_object(file)?;
    change(&mut obj);
    crate::fs_util::write_json_atomic(file, &Value::Object(obj), false)
}

/// A whole number field: `Some` for a JSON number that fits, an error naming the field otherwise.
pub fn whole(name: &str, value: &Value, min: u32, max: u32) -> Result<u32, Refused> {
    match value.as_u64().and_then(|n| u32::try_from(n).ok()) {
        Some(n) if (min..=max).contains(&n) => Ok(n),
        _ => Err(Refused(format!("{name} must be a whole number from {min} to {max}."))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_missing_file_is_empty_and_a_bad_one_is_an_error_that_names_it() {
        let dir = tempfile::tempdir().unwrap();
        let file = file_in(dir.path());
        assert!(read_object(&file).unwrap().is_empty());
        std::fs::write(&file, "{nope").unwrap();
        assert!(read_object(&file).unwrap_err().to_string().contains("settings.json"));
        std::fs::write(&file, "[1]").unwrap();
        assert!(read_object(&file).is_err());
    }

    #[test]
    fn a_patch_keeps_keys_it_does_not_touch_and_refuses_to_overwrite_a_bad_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("sub").join(FILE);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, r#"{"future":[1],"slots":1}"#).unwrap();
        patch(&file, |o| {
            o.insert("slots".into(), json!(3));
        })
        .unwrap();
        assert_eq!(read_object(&file).unwrap()["future"], json!([1]));
        assert_eq!(read_object(&file).unwrap()["slots"], json!(3));
        std::fs::write(&file, "{nope").unwrap();
        assert!(patch(&file, |_| {}).is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "{nope");
    }

    #[test]
    fn locked_names_the_fields_the_environment_sets_ignoring_blanks() {
        let fields = [
            Field {
                name: "a",
                env: "A_VAR",
            },
            Field {
                name: "b",
                env: "B_VAR",
            },
            Field {
                name: "c",
                env: "C_VAR",
            },
        ];
        let env = Env::from_pairs([("A_VAR", "x"), ("B_VAR", "  ")]);
        assert_eq!(locked(&env, &fields), ["a"]);
    }

    #[test]
    fn whole_numbers_are_range_checked() {
        assert_eq!(whole("slots", &json!(3), 0, 8), Ok(3));
        for bad in [json!(9), json!(-1), json!("3"), json!(1.5), json!(null)] {
            assert!(whole("slots", &bad, 0, 8).is_err(), "{bad}");
        }
    }
}
