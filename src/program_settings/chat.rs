//! The Chat Agent's settings: `<data folder>/chat/settings.json`, layered under the environment. The Chat Agent
//! learns its projects from the store (chat-agent-process.md section 5.1), so the file holds almost nothing: which one
//! project to serve when it should not serve all of them, and (`crate::secrets`) the tokens its Claude Code runs on.
//!
//! ```json
//! { "onlyRepoId": "123" }
//! ```
//!
//! `RETASKABLE_CHAT_REPO_ID` wins over the file. The agent reads it again at each refresh of its project list, so a
//! change applies within a few minutes without a restart.
use std::path::Path;

use anyhow::Result;
use serde_json::{Map, Value, json};

use super::{Field, Refused};
use crate::env::Env;

pub const ONLY_REPO: Field = Field {
    name: "onlyRepoId",
    env: "RETASKABLE_CHAT_REPO_ID",
};
/// The fields the page edits, with the variable that overrides each.
pub const FIELDS: [Field; 1] = [ONLY_REPO];

/// What the file says.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct File {
    pub only_repo_id: Option<String>,
}

/// A repository id as the store writes it: digits.
fn repo_id(v: &Value) -> Result<String, Refused> {
    let text = match v {
        Value::String(s) => s.trim().to_string(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    };
    if text.is_empty() || text.len() > 20 || !text.chars().all(|c| c.is_ascii_digit()) {
        return Err(Refused("onlyRepoId must be a repository id: digits only.".into()));
    }
    Ok(text)
}

impl File {
    pub fn parse(obj: &Map<String, Value>) -> Result<Self> {
        let only_repo_id = match obj.get(ONLY_REPO.name).filter(|v| !v.is_null()) {
            Some(v) => Some(repo_id(v).map_err(|r| anyhow::anyhow!(r.0))?),
            None => None,
        };
        Ok(Self { only_repo_id })
    }
}

/// The file in the Chat Agent's folder `dir` (`<data folder>/chat`); a missing file is an empty one.
pub fn read(dir: &Path) -> Result<File> {
    File::parse(&super::read_object(&super::file_in(dir))?)
}

/// What the agent uses and what is locked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Resolved {
    /// Serve only this repository id; `None` serves every project the store names.
    pub only_repo_id: Option<String>,
    pub locked: Vec<&'static str>,
}

pub fn resolve(env: &Env, file: &File) -> Resolved {
    let from_env = env
        .get(ONLY_REPO.env)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    Resolved {
        only_repo_id: from_env.or_else(|| file.only_repo_id.clone()),
        locked: super::locked(env, &FIELDS),
    }
}

/// The repository id to serve now: the environment's, else the file's. A file that can't be read leaves it unset, with
/// the reason for the agent's `lastError`.
pub fn only_repo(env: &Env, dir: &Path) -> (Option<String>, Option<String>) {
    match read(dir) {
        Ok(file) => (resolve(env, &file).only_repo_id, None),
        Err(e) => (resolve(env, &File::default()).only_repo_id, Some(format!("{e:#}"))),
    }
}

/// A save from the page, as for the CI runner's (`ci::apply`).
pub fn apply(dir: &Path, env: &Env, update: &Map<String, Value>) -> Result<(), Refused> {
    let mut change: Option<Option<Value>> = None;
    for (key, raw) in update {
        if key != ONLY_REPO.name {
            return Err(Refused("That isn't a setting the Chat Agent has.".into()));
        }
        if super::locked(env, &FIELDS).contains(&ONLY_REPO.name) {
            return Err(Refused(format!(
                "{} is set by the environment ({}): change it there.",
                ONLY_REPO.name, ONLY_REPO.env
            )));
        }
        change = Some(if raw.is_null() {
            None
        } else {
            Some(json!(repo_id(raw)?))
        });
    }
    let Some(value) = change else { return Ok(()) };
    super::patch(&super::file_in(dir), |obj| match value {
        Some(v) => {
            obj.insert(ONLY_REPO.name.to_string(), v);
        }
        None => {
            obj.remove(ONLY_REPO.name);
        }
    })
    .map_err(|e| Refused(format!("Couldn't save the Chat Agent's settings: {e}")))
}

/// What the page shows of the file in `dir`, shaped as the CI runner's (`ci::view`).
pub fn view(env: &Env, dir: &Path) -> Value {
    let (file, error) = match read(dir) {
        Ok(file) => (file, None),
        Err(e) => (File::default(), Some(format!("{e:#}"))),
    };
    let r = resolve(env, &file);
    json!({
        "file": { "onlyRepoId": file.only_repo_id },
        "effective": { "onlyRepoId": r.only_repo_id },
        "defaults": {},
        "locked": r.locked,
        "env": { ONLY_REPO.name: ONLY_REPO.env },
        "error": error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::program_settings::file_in;

    fn obj(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn the_environment_wins_over_the_file() {
        let file = File {
            only_repo_id: Some("7".into()),
        };
        assert_eq!(resolve(&Env::empty(), &file).only_repo_id.as_deref(), Some("7"));
        let env = Env::from_pairs([("RETASKABLE_CHAT_REPO_ID", " 2 ")]);
        let r = resolve(&env, &file);
        assert_eq!((r.only_repo_id.as_deref(), r.locked), (Some("2"), vec!["onlyRepoId"]));
        assert_eq!(resolve(&Env::empty(), &File::default()), Resolved::default());
    }

    #[test]
    fn a_save_writes_the_file_and_null_clears_it() {
        let dir = tempfile::tempdir().unwrap();
        apply(dir.path(), &Env::empty(), &obj(json!({"onlyRepoId": 12}))).unwrap();
        assert_eq!(read(dir.path()).unwrap().only_repo_id.as_deref(), Some("12"));
        apply(dir.path(), &Env::empty(), &obj(json!({"onlyRepoId": null}))).unwrap();
        assert_eq!(read(dir.path()).unwrap(), File::default());
    }

    #[test]
    fn a_locked_unknown_or_bad_save_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::from_pairs([("RETASKABLE_CHAT_REPO_ID", "3")]);
        assert!(apply(dir.path(), &env, &obj(json!({"onlyRepoId": "4"}))).is_err());
        for bad in [
            json!({"onlyRepoId": "x1"}),
            json!({"onlyRepoId": ""}),
            json!({"claude": 1}),
        ] {
            assert!(apply(dir.path(), &Env::empty(), &obj(bad.clone())).is_err(), "{bad}");
        }
        assert!(!file_in(dir.path()).exists());
    }

    #[test]
    fn a_bad_file_keeps_the_environment_value_and_reports() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(file_in(dir.path()), "{oops").unwrap();
        let env = Env::from_pairs([("RETASKABLE_CHAT_REPO_ID", "3")]);
        let (only, error) = only_repo(&env, dir.path());
        assert_eq!(only.as_deref(), Some("3"));
        assert!(error.unwrap().contains("settings.json"));
    }
}
