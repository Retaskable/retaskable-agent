//! The CI runner's settings (docs/specs/agent-ci.md section 3.7): `<data folder>/ci/settings.json`, layered under the
//! environment. The runner (`src/agent_ci`) reads it every poll, and the settings page edits it; both import this,
//! neither imports the other.
//!
//! ```json
//! { "repos": ["owner/repo", "owner/other@trunk"], "slots": 2, "cpus": 8, "name": "build-box" }
//! ```
//!
//! Every field is optional. `AGENT_CI_REPO`, `AGENT_CI_SLOTS`, `AGENT_CI_CPUS` and `AGENT_CI_NAME` win over the file,
//! each on its own.
use std::path::Path;

use anyhow::{Result, bail};
use serde_json::{Map, Value, json};

use super::{Field, Refused};
use crate::env::Env;

/// Slots when neither `AGENT_CI_SLOTS` nor the file says.
pub const DEFAULT_SLOTS: u32 = 2;
/// CPUs a check may use when nothing says, before the cap at what Docker has.
pub const DEFAULT_CPUS: u32 = 8;
/// The branch checked when a repo names none.
pub const DEFAULT_BASE: &str = "main";
/// What the page may set a field to: a typing slip (200 slots) is refused, not run.
const MAX_SLOTS: u32 = 64;
const MAX_CPUS: u32 = 256;
const MAX_NAME: usize = 100;

pub const REPOS: Field = Field {
    name: "repos",
    env: "AGENT_CI_REPO",
};
pub const SLOTS: Field = Field {
    name: "slots",
    env: "AGENT_CI_SLOTS",
};
pub const CPUS: Field = Field {
    name: "cpus",
    env: "AGENT_CI_CPUS",
};
pub const NAME: Field = Field {
    name: "name",
    env: "AGENT_CI_NAME",
};
/// The fields the page edits, with the variable that overrides each.
pub const FIELDS: [Field; 4] = [REPOS, SLOTS, CPUS, NAME];

/// One GitHub repo CI runs for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    /// `owner/repo`.
    pub github_repo: String,
    /// The branch pull requests merge into, and whose own pushes are checked.
    pub base_branch: String,
}

impl Repo {
    /// `owner/repo`, or `owner/repo@branch` for a base other than `main`.
    pub fn parse(spec: &str) -> Result<Self> {
        let (name, base) = spec.split_once('@').unwrap_or((spec, DEFAULT_BASE));
        let ok = |part: &str| {
            !part.is_empty()
                && part.chars().any(|c| c != '.')
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        };
        match name.split_once('/') {
            Some((owner, repo)) if ok(owner) && ok(repo) && !base.trim().is_empty() => Ok(Self {
                github_repo: name.to_string(),
                base_branch: base.trim().to_string(),
            }),
            _ => bail!("{spec:?} is not a GitHub repo: write owner/repo, or owner/repo@branch"),
        }
    }

    /// The name its folders and logs go by: `owner-repo`.
    pub fn id(&self) -> String {
        self.github_repo.replace('/', "-")
    }

    /// The URL its bare mirror fetches from.
    pub fn url(&self) -> String {
        format!("https://github.com/{}.git", self.github_repo)
    }

    /// The text `parse` reads back.
    pub fn spec(&self) -> String {
        if self.base_branch == DEFAULT_BASE {
            self.github_repo.clone()
        } else {
            format!("{}@{}", self.github_repo, self.base_branch)
        }
    }
}

/// The comma-separated `AGENT_CI_REPO`.
pub fn parse_repos(list: &str) -> Result<Vec<Repo>> {
    let mut repos: Vec<Repo> = Vec::new();
    for spec in list.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let repo = Repo::parse(spec)?;
        if !repos.contains(&repo) {
            repos.push(repo);
        }
    }
    Ok(repos)
}

/// What the file says; a field it leaves out is `None` (or no repos).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct File {
    pub repos: Vec<Repo>,
    pub slots: Option<u32>,
    pub cpus: Option<u32>,
    pub name: Option<String>,
}

impl File {
    /// The file's object read strictly: a field of the wrong type or out of range fails the whole read, so a reader
    /// keeps the values it had rather than half-applying a file with a slip in it.
    pub fn parse(obj: &Map<String, Value>) -> Result<Self> {
        let mut file = Self::default();
        if let Some(v) = obj.get(REPOS.name).filter(|v| !v.is_null()) {
            file.repos = repos_from(v).map_err(|r| anyhow::anyhow!(r.0))?;
        }
        if let Some(v) = obj.get(SLOTS.name).filter(|v| !v.is_null()) {
            file.slots = Some(super::whole(SLOTS.name, v, 0, MAX_SLOTS).map_err(|r| anyhow::anyhow!(r.0))?);
        }
        if let Some(v) = obj.get(CPUS.name).filter(|v| !v.is_null()) {
            file.cpus = Some(super::whole(CPUS.name, v, 1, MAX_CPUS).map_err(|r| anyhow::anyhow!(r.0))?);
        }
        if let Some(v) = obj.get(NAME.name).filter(|v| !v.is_null()) {
            file.name = Some(name_from(v).map_err(|r| anyhow::anyhow!(r.0))?);
        }
        Ok(file)
    }
}

fn repos_from(v: &Value) -> Result<Vec<Repo>, Refused> {
    let Some(items) = v.as_array() else {
        return Err(Refused("repos must be a list of owner/repo.".into()));
    };
    let mut repos: Vec<Repo> = Vec::new();
    for item in items {
        let spec = item
            .as_str()
            .ok_or_else(|| Refused("repos must be a list of owner/repo.".into()))?;
        let repo = Repo::parse(spec.trim()).map_err(|e| Refused(e.to_string()))?;
        if !repos.contains(&repo) {
            repos.push(repo);
        }
    }
    Ok(repos)
}

fn name_from(v: &Value) -> Result<String, Refused> {
    match v.as_str().map(str::trim) {
        Some(n) if !n.is_empty() && n.chars().count() <= MAX_NAME && !n.chars().any(char::is_control) => {
            Ok(n.to_string())
        }
        _ => Err(Refused(format!("name must be 1 to {MAX_NAME} characters on one line."))),
    }
}

/// The file in the runner's folder `dir` (`<data folder>/ci`). A missing file is an empty one; one that can't be read
/// or has a bad field is an error.
pub fn read(dir: &Path) -> Result<File> {
    File::parse(&super::read_object(&super::file_in(dir))?)
}

/// What the runner uses: each field from the environment, else the file, else the default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub repos: Vec<Repo>,
    pub slots: u32,
    pub cpus: u32,
    /// `None`: the host name, which the runner finds.
    pub name: Option<String>,
    /// The fields the environment set, so the page shows them locked.
    pub locked: Vec<&'static str>,
}

fn number(env: &Env, field: &Field, from_file: Option<u32>, default: u32) -> Result<u32> {
    match env.get(field.env).filter(|v| !v.trim().is_empty()) {
        None => Ok(from_file.unwrap_or(default)),
        Some(v) => v
            .trim()
            .parse()
            .map_err(|_| anyhow::anyhow!("{} is {v:?}: it must be a whole number", field.env)),
    }
}

/// Layers `env` over `file`. No repo from either is an error: a runner with nothing to check has misread its setup.
pub fn resolve(env: &Env, file: &File) -> Result<Resolved> {
    let repos = match env.get(REPOS.env).filter(|v| !v.trim().is_empty()) {
        Some(list) => parse_repos(&list)?,
        None => file.repos.clone(),
    };
    if repos.is_empty() {
        bail!(
            "No repo to check: set {} to owner/repo (comma-separated for several), or list them on the settings page",
            REPOS.env
        );
    }
    let name = match env
        .get(NAME.env)
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
    {
        Some(n) => Some(n),
        None => file.name.clone(),
    };
    Ok(Resolved {
        repos,
        slots: number(env, &SLOTS, file.slots, DEFAULT_SLOTS)?,
        cpus: number(env, &CPUS, file.cpus, DEFAULT_CPUS)?.max(1),
        name,
        locked: super::locked(env, &FIELDS),
    })
}

/// A save from the page: the fields named in `update` (a value, or `null` to go back to the default) written to the
/// file in `dir`, if every one passes. A field the environment sets is refused, and so is a name this page doesn't
/// set, so nothing is written for a request that is partly wrong.
pub fn apply(dir: &Path, env: &Env, update: &Map<String, Value>) -> Result<(), Refused> {
    let mut changes: Vec<(&'static str, Option<Value>)> = Vec::new();
    for (key, raw) in update {
        let Some(field) = FIELDS.iter().find(|f| f.name == key) else {
            // Not echoed: a request's own text is never put in a reply.
            return Err(Refused("That isn't a setting the CI runner has.".into()));
        };
        if super::locked(env, std::slice::from_ref(field)).contains(&field.name) {
            return Err(Refused(format!(
                "{} is set by the environment ({}): change it there.",
                field.name, field.env
            )));
        }
        let value = if raw.is_null() {
            None
        } else {
            Some(match field.name {
                "repos" => {
                    let repos = repos_from(raw)?;
                    // An empty list is the default: no repos.
                    (!repos.is_empty()).then(|| json!(repos.iter().map(Repo::spec).collect::<Vec<_>>()))
                }
                "slots" => Some(json!(super::whole(SLOTS.name, raw, 0, MAX_SLOTS)?)),
                "cpus" => Some(json!(super::whole(CPUS.name, raw, 1, MAX_CPUS)?)),
                _ => Some(json!(name_from(raw)?)),
            })
            .flatten()
        };
        changes.push((field.name, value));
    }
    if changes.is_empty() {
        return Ok(());
    }
    super::patch(&super::file_in(dir), |obj| {
        for (name, value) in changes {
            match value {
                Some(v) => obj.insert(name.to_string(), v),
                None => obj.remove(name),
            };
        }
    })
    .map_err(|e| Refused(format!("Couldn't save the CI runner's settings: {e}")))
}

#[cfg(test)]
mod tests;
