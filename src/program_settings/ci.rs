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
//!
//! The file also holds a `network` key (`{ "enabled": false, "passwordHash": "scrypt$..." }`, off by default) when this
//! runner hosts the settings page on a computer with no code agent (agent-ci.md section 3.8). It is set by
//! `retaskable-ci set-password` and `network`, never by the page, and the page's view never carries it.
use std::path::Path;

use anyhow::{Result, bail};
use serde_json::{Map, Value, json};

use super::{Field, Refused};
use crate::env::Env;
use crate::network::{MIN_PASSWORD_LEN, NetworkAccess};

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

/// The `network` key: whether another computer may reach the runner's own settings page, and the scrypt hash of the
/// password it signs in with. Off, with no password, when the key is missing. A key that isn't an object, or a file
/// that can't be read, is an error (the page then stays shut: fail closed).
pub fn network(dir: &Path) -> Result<NetworkAccess> {
    match super::read_object(&super::file_in(dir))?.get("network") {
        None | Some(Value::Null) => Ok(NetworkAccess::default()),
        Some(v) => NetworkAccess::from_value(v).ok_or_else(|| anyhow::anyhow!("network must be an object.")),
    }
}

/// Sets the sign-in password: at least `MIN_PASSWORD_LEN` characters, kept only as its scrypt hash. Whether network
/// access is on is left as it was.
pub fn set_password(dir: &Path, password: &str) -> Result<(), Refused> {
    if password.encode_utf16().count() < MIN_PASSWORD_LEN {
        return Err(Refused(format!(
            "A password needs at least {MIN_PASSWORD_LEN} characters."
        )));
    }
    let hash = crate::password::hash_password(password);
    write_network(dir, |access| access.password_hash = Some(hash))
}

/// Turns network access on or off, keeping the hash either way. On needs a password to have been set.
pub fn set_network(dir: &Path, on: bool) -> Result<(), Refused> {
    let has_password = network(dir).is_ok_and(|a| a.password_hash.is_some());
    if on && !has_password {
        return Err(Refused(
            "Network access needs a password first: run `retaskable-ci set-password`.".into(),
        ));
    }
    write_network(dir, |access| access.enabled = on)
}

fn write_network(dir: &Path, change: impl FnOnce(&mut NetworkAccess)) -> Result<(), Refused> {
    let refused = |e: anyhow::Error| Refused(format!("Couldn't save the CI runner's settings: {e:#}"));
    let mut access = network(dir).map_err(refused)?;
    change(&mut access);
    super::patch(&super::file_in(dir), |obj| {
        obj.insert("network".into(), access.to_value());
    })
    .map_err(refused)
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

/// What the page shows of the file in `dir`: each field as the file has it (what a form edits), as the runner will use
/// it (the environment over the file over the default), which fields the environment sets, and why the file can't be
/// read, if it can't. No secret is here (`crate::secrets` has those).
pub fn view(env: &Env, dir: &Path) -> Value {
    let (file, error) = match read(dir) {
        Ok(file) => (file, None),
        Err(e) => (File::default(), Some(format!("{e:#}"))),
    };
    let specs = |repos: &[Repo]| repos.iter().map(Repo::spec).collect::<Vec<_>>();
    let from_env = |field: &Field| env.get(field.env).filter(|v| !v.trim().is_empty());
    let repos = from_env(&REPOS)
        .and_then(|list| parse_repos(&list).ok())
        .map_or_else(|| specs(&file.repos), |r| specs(&r));
    let num = |field: &Field, own: Option<u32>, default: u32| {
        from_env(field)
            .and_then(|v| v.trim().parse::<u32>().ok())
            .or(own)
            .unwrap_or(default)
    };
    json!({
        "file": { "repos": specs(&file.repos), "slots": file.slots, "cpus": file.cpus, "name": file.name },
        "effective": {
            "repos": repos,
            "slots": num(&SLOTS, file.slots, DEFAULT_SLOTS),
            "cpus": num(&CPUS, file.cpus, DEFAULT_CPUS),
            "name": from_env(&NAME).map(|n| n.trim().to_string()).or(file.name),
        },
        "defaults": { "slots": DEFAULT_SLOTS, "cpus": DEFAULT_CPUS },
        "locked": super::locked(env, &FIELDS),
        "env": FIELDS.iter().map(|f| (f.name, f.env)).collect::<std::collections::BTreeMap<_, _>>(),
        "error": error,
    })
}

#[cfg(test)]
mod tests;
