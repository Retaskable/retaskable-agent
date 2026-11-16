//! The tokens the agent's tools read from the environment (agent-settings.md section 3.3): GitHub's, Claude Code's,
//! GitLab's and Cloudflare's. One saved on the settings page (or by `secret set`) wins over the same environment
//! variable and is put in this process's environment, which every session, check and script it starts inherits; the
//! environment fills only a secret that was never saved, and is what a deleted saved one goes back to (decided
//! 2026-11-16, agent-settings.md section 3.3). A saved one lives in
//! `config.json`, under `secrets`, as ciphertext (`cipher.rs`) under a key kept apart from the config (`key.rs`).
//! The page shows each as set or not and a short fingerprint, never the value; the site gets names and source only.
//! A value that can't be opened is "unreadable", never plaintext, never a crash.
//!
//! Each program keeps its own (`Owner`): the code agent in `config.json`, the CI runner and the Chat Agent in the
//! `secrets` map of their own `settings.json` (`program_settings`). All three are sealed under the one data key of the
//! data folder, and each program only ever opens the names it uses.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::config;
use crate::env::Env;
use crate::link::{self, TokenStore};
use crate::sync::lock;

mod cipher;
pub mod cli;
#[cfg(test)]
pub(crate) mod fakes;
mod host;
mod key;
pub mod names;

pub use cipher::{DataKey, Unreadable, open, seal};
pub use host::{Host, NoHost, ProcessHost};
pub use key::{KEY_FILE_VAR, KEY_SERVICE, KEY_VAR, Keys, account_for};

pub struct SecretDef {
    pub name: &'static str,
    pub label: &'static str,
    pub usage: &'static str,
}

pub const SECRETS: [SecretDef; 6] = [
    SecretDef {
        name: "GH_TOKEN",
        label: "GitHub",
        usage: "gh, and git pushes over https",
    },
    SecretDef {
        name: "CLAUDE_CODE_OAUTH_TOKEN",
        label: "Claude Code token",
        usage: "Signs Claude Code in (claude setup-token)",
    },
    SecretDef {
        name: "ANTHROPIC_API_KEY",
        label: "Anthropic API key",
        usage: "Claude Code on an API key instead",
    },
    SecretDef {
        name: "GITLAB_TOKEN",
        label: "GitLab",
        usage: "glab, for GitLab projects",
    },
    SecretDef {
        name: "CLOUDFLARE_ACCOUNT_ID",
        label: "Cloudflare account",
        usage: "Releasing retaskable.com (the rollout scripts)",
    },
    SecretDef {
        name: "CLOUDFLARE_API_TOKEN",
        label: "Cloudflare API token",
        usage: "Releasing retaskable.com; sessions never get it",
    },
];

/// What the CI runner reads from its environment and can have saved: its `gh` calls use `GH_TOKEN`.
pub const CI_SECRETS: [SecretDef; 1] = [SecretDef {
    name: "GH_TOKEN",
    label: "GitHub",
    usage: "gh, for the pull requests and statuses CI reads and writes",
}];

/// What the Chat Agent's Claude Code can be signed in with, besides its own login.
pub const CHAT_SECRETS: [SecretDef; 2] = [
    SecretDef {
        name: "CLAUDE_CODE_OAUTH_TOKEN",
        label: "Claude Code token",
        usage: "Signs Claude Code in (claude setup-token)",
    },
    SecretDef {
        name: "ANTHROPIC_API_KEY",
        label: "Anthropic API key",
        usage: "Claude Code on an API key instead",
    },
];

/// Which program a set of saved secrets belongs to, and so which file holds them and which names it takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    CodeAgent,
    ChatAgent,
    CiRunner,
}

impl Owner {
    pub fn defs(self) -> &'static [SecretDef] {
        match self {
            Owner::CodeAgent => &SECRETS,
            Owner::ChatAgent => &CHAT_SECRETS,
            Owner::CiRunner => &CI_SECRETS,
        }
    }

    /// The file whose `secrets` map holds them, under the data folder `root`.
    pub fn file(self, root: &Path) -> PathBuf {
        match self {
            Owner::CodeAgent => root.join("config.json"),
            Owner::ChatAgent => crate::program_settings::file_in(&crate::paths::chat_dir(root)),
            Owner::CiRunner => crate::program_settings::file_in(&crate::paths::ci_dir(root)),
        }
    }
}

/// The TypeScript agent's credential store service for a saved secret, by its variable's name. Read once, to move what
/// a computer saved there into the config.
const LEGACY_SERVICE: &str = "retaskable-secret";
/// The names this process put in its environment from the saved ones, so a restarted agent (which inherits the
/// environment) still tells them from ones the environment set. Names only.
const SAVED_VAR: &str = "RETASKABLE_SAVED_SECRETS";
const MAX_LEN: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Env,
    Saved,
    Unset,
    /// Saved, but it doesn't open under this computer's key: save it again.
    Unreadable,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Env => "env",
            Source::Saved => "saved",
            Source::Unset => "unset",
            Source::Unreadable => "unreadable",
        }
    }
}

pub struct SecretState {
    pub name: String,
    /// The program's known secret this is, with its label and use; `None` for a variable a person added.
    pub def: Option<&'static SecretDef>,
    pub source: Source,
    /// The first 8 hex characters of the value's SHA-256, to tell which one is in use. Never sent to the site.
    pub fingerprint: Option<String>,
    /// The program's environment set it when it started, so deleting the saved one goes back to that value.
    pub env_set: bool,
}

pub fn fingerprint(value: &str) -> String {
    Sha256::digest(value.as_bytes())[..4]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A save refused, in words for the page. Names only, never a value.
#[derive(Debug, PartialEq, Eq)]
pub struct Refused(pub String);

/// A save checked and sealed but not yet written (`Secrets::plan`), so the page's other settings can be checked
/// before any of it is. It holds the values: no `Debug`.
pub struct Plan {
    items: Vec<(String, Option<Applied>)>,
}

struct Applied {
    value: Zeroizing<String>,
    sealed: String,
}

impl Plan {
    pub fn names(&self) -> Vec<&str> {
        self.items.iter().map(|(name, _)| name.as_str()).collect()
    }
}

#[derive(Default)]
struct Inner {
    /// The names the environment set when the agent started: what a secret that isn't saved comes from.
    from_env: BTreeSet<String>,
    /// Those names' values as the process started with them (only a process has them), to go back to when a saved one
    /// is deleted. Never shown.
    env_values: BTreeMap<String, Zeroizing<String>>,
    /// The names this agent put in the environment from the saved ones.
    saved: BTreeSet<String>,
    /// The file's `secrets` as `apply` or `refresh` last read them, so `refresh` is a file read when nothing changed.
    seen: BTreeMap<String, String>,
}

pub struct Secrets {
    /// The data folder: the key's account is a hash of it, whichever program's secrets these are.
    dir: PathBuf,
    owner: Owner,
    keys: Keys,
    host: Arc<dyn Host>,
    /// The TypeScript agent's store, for the move at start; only the code agent has one.
    legacy: Option<Arc<dyn TokenStore>>,
    inner: Mutex<Inner>,
}

impl Secrets {
    /// The agent's own: this process's environment, the OS credential store for the key, and wherever the TypeScript
    /// agent kept its secrets on this computer.
    pub fn for_process(dir: &Path, env: &Env) -> Arc<Self> {
        let legacy = link::keychain_for(dir, env, LEGACY_SERVICE, "RETASKABLE_CREDENTIALS_FILE");
        Arc::new(Self::new(
            dir,
            env,
            Arc::new(ProcessHost),
            link::os_store(KEY_SERVICE),
            legacy,
        ))
    }

    pub fn new(
        dir: &Path,
        env: &Env,
        host: Arc<dyn Host>,
        os: Option<Arc<dyn TokenStore>>,
        legacy: Arc<dyn TokenStore>,
    ) -> Self {
        Self::build(Owner::CodeAgent, dir, env, host, os, Some(legacy))
    }

    fn build(
        owner: Owner,
        dir: &Path,
        env: &Env,
        host: Arc<dyn Host>,
        os: Option<Arc<dyn TokenStore>>,
        legacy: Option<Arc<dyn TokenStore>>,
    ) -> Self {
        Self {
            dir: dir.to_path_buf(),
            owner,
            keys: Keys::open(dir, env, os),
            host,
            legacy,
            inner: Mutex::new(Inner::default()),
        }
    }

    /// The Chat Agent's or the CI runner's own, run by that program: its process environment, and the OS credential
    /// store for the one key of the data folder `root`. Call `apply` once at start.
    pub fn for_program(owner: Owner, root: &Path, env: &Env) -> Arc<Self> {
        Arc::new(Self::build(
            owner,
            root,
            env,
            Arc::new(ProcessHost),
            link::os_store(KEY_SERVICE),
            None,
        ))
    }

    /// A program's secrets as the settings page edits them when that program runs elsewhere, or as another process:
    /// the saved ones are read and sealed, and saving touches nothing but the program's file. Never this process's
    /// environment (a token saved for the CI runner must not become the code agent's own), so `env_set` says which
    /// names the program's environment sets, as its status file reported them: shown as the environment's when nothing
    /// is saved.
    pub fn for_page(owner: Owner, root: &Path, env: &Env, os: Option<Arc<dyn TokenStore>>, env_set: &[String]) -> Self {
        let this = Self::build(owner, root, env, Arc::new(host::NoHost), os, None);
        lock(&this.inner).from_env = env_set
            .iter()
            .filter(|n| owner.defs().iter().any(|d| d.name == n.as_str()) || names::check_name(n).is_ok())
            .cloned()
            .collect();
        this
    }

    /// The names this process's environment set when it started (saved or not), noted by `apply`: what a program tells
    /// its status file.
    pub fn env_names(&self) -> Vec<String> {
        lock(&self.inner).from_env.iter().cloned().collect()
    }

    /// Why a save can't work on this computer (no key to seal with), or `None`.
    pub fn save_problem(&self) -> Option<String> {
        self.keys.save_problem()
    }

    /// At start, before anything is spawned: moves the TypeScript agent's saved secrets in, notes which names the
    /// environment set (an empty value is none) and their values, and puts the saved ones in the environment for the
    /// rest, over what it had. A saved one that can't be opened is removed from the environment, never left to the
    /// environment's value: it fails closed, and the page says it's unreadable.
    pub fn apply(&self) {
        // The key must not reach a child: a session could open the config with it. A restart by a supervisor (Docker,
        // systemd) brings its own copy back; `RETASKABLE_SECRETS_KEY_FILE` doesn't depend on that.
        self.host.remove(KEY_VAR);
        self.migrate();
        let before: BTreeSet<String> = self
            .host
            .get(SAVED_VAR)
            .map(|v| v.split(',').filter(|n| !n.is_empty()).map(str::to_string).collect())
            .unwrap_or_default();
        let stored = self.stored();
        let key = self.keys.for_reading();
        let mut inner = lock(&self.inner);
        inner.seen = stored.clone();
        inner.from_env.clear();
        inner.env_values.clear();
        inner.saved.clear();
        for name in self.names_in(&stored, &BTreeSet::new()) {
            let name = name.as_str();
            let saved = read_saved(name, &stored, &key);
            let env_value = self
                .host
                .get(name)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty());
            // Put here from the saved one by the agent before a restart (and unchanged since): still the saved one.
            if let Some(value) = &env_value
                && !(before.contains(name) && matches!(&saved, Some(Ok(plain)) if **plain == *value))
            {
                inner.from_env.insert(name.to_string());
                inner.env_values.insert(name.to_string(), Zeroizing::new(value.clone()));
            }
            match saved {
                Some(Ok(plain)) => {
                    self.host.set(name, &plain);
                    inner.saved.insert(name.to_string());
                }
                Some(Err(Unreadable)) => {
                    tracing::warn!("The saved {name} can't be read with this computer's key: save it again.");
                    self.host.remove(name);
                }
                None => self.restore_env(&inner, name),
            }
        }
        self.mark_saved(&inner);
        if inner.saved.contains("GH_TOKEN") {
            self.host.git_login();
        }
    }

    /// For a program that runs on while the page (another process) saves its secrets: reads the file again and, if
    /// its `secrets` changed since `apply` or the last `refresh`, brings the environment new children inherit in line
    /// with it: a saved one replaces the environment's, and a deleted one goes back to the environment's value from
    /// start. Sessions already running keep the environment they had.
    pub fn refresh(&self) {
        let stored = self.stored();
        if lock(&self.inner).seen == stored {
            return;
        }
        let key = self.keys.for_reading();
        let mut inner = lock(&self.inner);
        let mut gh_changed = false;
        // A custom variable another process deleted is no longer in the file, so the ones this one put in too.
        for name in self.names_in(&stored, &inner.saved) {
            let name = name.as_str();
            match read_saved(name, &stored, &key) {
                Some(Ok(plain)) => {
                    gh_changed |= name == "GH_TOKEN" && self.host.get(name).as_deref() != Some(&**plain);
                    self.note_env(&mut inner, name);
                    self.host.set(name, &plain);
                    inner.saved.insert(name.to_string());
                }
                Some(Err(Unreadable)) => {
                    tracing::warn!("The saved {name} can't be read with this computer's key: save it again.");
                    self.host.remove(name);
                    inner.saved.remove(name);
                }
                None => {
                    gh_changed |= name == "GH_TOKEN" && inner.saved.contains(name) && inner.from_env.contains(name);
                    self.restore_env(&inner, name);
                    inner.saved.remove(name);
                }
            }
        }
        inner.seen = stored;
        self.mark_saved(&inner);
        if gh_changed {
            self.host.git_login();
        }
    }

    /// The program's known secrets, in order, then each variable a person added that is saved, by name.
    pub fn states(&self) -> Vec<SecretState> {
        let stored = self.stored();
        let key = self.keys.for_reading();
        let inner = lock(&self.inner);
        let known = |name: &str| self.owner.defs().iter().any(|d| d.name == name);
        self.names_in(&stored, &BTreeSet::new())
            .into_iter()
            .filter(|name| known(name) || stored.contains_key(name))
            .map(|name| {
                let env_set = inner.from_env.contains(&name);
                let (source, value) = match read_saved(&name, &stored, &key) {
                    Some(Ok(plain)) => (Source::Saved, Some(plain)),
                    Some(Err(Unreadable)) => (Source::Unreadable, None),
                    None if env_set => (
                        Source::Env,
                        inner
                            .env_values
                            .get(&name)
                            .cloned()
                            .or_else(|| self.host.get(&name).map(Zeroizing::new)),
                    ),
                    None => (Source::Unset, None),
                };
                SecretState {
                    def: self.owner.defs().iter().find(|d| d.name == name),
                    name,
                    source,
                    fingerprint: value.map(|v| fingerprint(&v)),
                    env_set,
                }
            })
            .collect()
    }

    /// The names this program handles: its known ones, then the custom ones in the file that are fit to be set (a
    /// hand-edited name the rules refuse is left alone, with a warning) and `extra` (names this process set before).
    fn names_in(&self, stored: &BTreeMap<String, String>, extra: &BTreeSet<String>) -> Vec<String> {
        let mut names: Vec<String> = self.owner.defs().iter().map(|d| d.name.to_string()).collect();
        let mut custom: BTreeSet<String> = BTreeSet::new();
        for name in stored.keys().chain(extra) {
            if names.iter().any(|n| n == name) {
                continue;
            }
            if names::check_name(name).is_ok() {
                custom.insert(name.clone());
            } else if stored.contains_key(name) {
                tracing::warn!(
                    "Ignoring a saved variable whose name the rules refuse ({name}): delete it from the file."
                );
            }
        }
        names.extend(custom);
        names
    }

    /// Checks every change (a value to save, or `null` to clear) and seals the values, writing nothing. A name that
    /// isn't one of the program's known secrets is a variable a person adds: it must pass `names::check_name`.
    pub fn plan(&self, changes: &Map<String, Value>) -> Result<Plan, Refused> {
        let stored = self.stored();
        let known = |name: &str| self.owner.defs().iter().any(|d| d.name == name);
        let mut items: Vec<(String, Option<Zeroizing<String>>)> = Vec::new();
        for (name, raw) in changes {
            let custom = !known(name);
            if custom && !(raw.is_null() && stored.contains_key(name)) {
                names::check_name(name).map_err(Refused)?;
                // The same variable written another way (Windows ignores case), in this save or already saved.
                let twin = |other: &str| other != name && names::same(other, name);
                if let Some(d) = self.owner.defs().iter().find(|d| twin(d.name)) {
                    return Err(Refused(format!("{name} is the same variable as {}.", d.name)));
                }
                if changes.keys().any(|k| twin(k)) || stored.keys().any(|k| twin(k) && !changes.contains_key(k)) {
                    return Err(Refused(format!(
                        "{name} is there twice (a name's case doesn't tell variables apart)."
                    )));
                }
            }
            let value = match raw {
                Value::Null => {
                    items.push((name.clone(), None));
                    continue;
                }
                Value::String(text) => text.trim(),
                _ => return Err(Refused(format!("{name} must be text."))),
            };
            if custom {
                names::check_value(name, value).map_err(Refused)?;
            } else {
                if value.is_empty() || value.chars().count() > MAX_LEN {
                    return Err(Refused(format!("{name} must be 1 to {MAX_LEN} characters.")));
                }
                if value.chars().any(|c| c.is_whitespace() || c.is_control()) {
                    return Err(Refused(format!("{name} can't hold spaces or line breaks.")));
                }
            }
            items.push((name.clone(), Some(Zeroizing::new(value.to_string()))));
        }
        let kept = stored
            .keys()
            .filter(|k| !known(k) && !items.iter().any(|(n, _)| n == *k))
            .count();
        let added = items.iter().filter(|(n, v)| !known(n) && v.is_some()).count();
        if kept + added > names::MAX_CUSTOM {
            return Err(Refused(format!(
                "At most {} variables beyond the known secrets can be saved.",
                names::MAX_CUSTOM
            )));
        }
        // The key is made only for a save that has something to seal.
        let key = if items.iter().any(|(_, v)| v.is_some()) {
            Some(self.keys.for_saving().map_err(Refused)?)
        } else {
            None
        };
        let items = items
            .into_iter()
            .map(|(name, value)| {
                let applied = value.zip(key.as_ref()).map(|(value, key)| Applied {
                    sealed: seal(key, &name, &value),
                    value,
                });
                (name, applied)
            })
            .collect();
        Ok(Plan { items })
    }

    /// Writes a checked plan: into the config, then the environment new sessions inherit (a deleted one goes back to
    /// the environment's value from start, or none). Sessions already running keep the environment they started with.
    pub fn commit(&self, plan: Plan) -> Result<()> {
        if plan.items.is_empty() {
            return Ok(());
        }
        let mut inner = lock(&self.inner);
        self.write(|obj| {
            let entry = obj.entry("secrets").or_insert_with(|| json!({}));
            if !entry.is_object() {
                *entry = json!({});
            }
            let map = entry.as_object_mut().expect("made an object above");
            for (name, applied) in &plan.items {
                match applied {
                    Some(a) => map.insert(name.clone(), json!(a.sealed)),
                    None => map.remove(name),
                };
            }
            if map.is_empty() {
                obj.remove("secrets");
            }
        })
        .context("saving secrets to the config")?;
        let mut gh_changed = false;
        for (name, applied) in &plan.items {
            match applied {
                Some(a) => {
                    self.note_env(&mut inner, name);
                    self.host.set(name, &a.value);
                    inner.saved.insert(name.clone());
                    gh_changed |= name == "GH_TOKEN";
                }
                None => {
                    self.restore_env(&inner, name);
                    let was_saved = inner.saved.remove(name);
                    gh_changed |= name == "GH_TOKEN" && was_saved && inner.from_env.contains(name);
                }
            }
        }
        self.mark_saved(&inner);
        tracing::info!("Saved secrets changed: {}.", plan.names().join(", "));
        if gh_changed {
            self.host.git_login();
        }
        Ok(())
    }

    /// Changes the owner's file whole or not at all: `config.json` for the code agent, `settings.json` for the others.
    fn write(&self, change: impl FnOnce(&mut Map<String, Value>)) -> Result<()> {
        match self.owner {
            Owner::CodeAgent => config::save_machine_settings(&self.dir, change),
            _ => crate::program_settings::patch(&self.owner.file(&self.dir), change),
        }
    }

    /// A variable a person added while the program runs: the environment's value it has now, if any, is the one to go
    /// back to when the saved one is deleted (a known secret's was noted at start).
    fn note_env(&self, inner: &mut Inner, name: &str) {
        let known = self.owner.defs().iter().any(|d| d.name == name);
        if known || inner.saved.contains(name) || inner.env_values.contains_key(name) {
            return;
        }
        if let Some(value) = self
            .host
            .get(name)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
        {
            inner.from_env.insert(name.to_string());
            inner.env_values.insert(name.to_string(), Zeroizing::new(value));
        }
    }

    /// With nothing saved for `name`: the environment's value from start, or none.
    fn restore_env(&self, inner: &Inner, name: &str) {
        match inner.env_values.get(name) {
            Some(value) => self.host.set(name, value),
            None => self.host.remove(name),
        }
    }

    fn mark_saved(&self, inner: &Inner) {
        if inner.saved.is_empty() {
            self.host.remove(SAVED_VAR);
        } else {
            self.host
                .set(SAVED_VAR, &inner.saved.iter().cloned().collect::<Vec<_>>().join(","));
        }
    }

    /// The config's `secrets`, name to stored ciphertext. A config that can't be read has none here: loading it fails
    /// loudly elsewhere.
    fn stored(&self) -> BTreeMap<String, String> {
        let text = std::fs::read_to_string(self.owner.file(&self.dir)).unwrap_or_default();
        let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(&text) else {
            return BTreeMap::new();
        };
        obj.get("secrets")
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// For each name the config has no entry for but the TypeScript agent's store does: seals it into the config, then
    /// deletes the old entry. Logged once. Without a key to seal with, the old entry stays where it is.
    fn migrate(&self) {
        let Some(legacy) = &self.legacy else { return };
        let stored = self.stored();
        let mut found = Vec::new();
        for def in self.owner.defs().iter().filter(|d| !stored.contains_key(d.name)) {
            match legacy.get(def.name) {
                Ok(Some(value)) if !value.trim().is_empty() => found.push((def.name, value)),
                Ok(_) => {}
                Err(e) => tracing::warn!("Couldn't read the old saved {}: {e:#}", def.name),
            }
        }
        if found.is_empty() {
            return;
        }
        let names = found.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", ");
        let key = match self.keys.for_saving() {
            Ok(key) => key,
            Err(note) => {
                tracing::warn!("Left the saved {names} where they are, not in the config: {note}");
                return;
            }
        };
        let sealed: Vec<(&str, String)> = found.iter().map(|(n, v)| (*n, seal(&key, n, v))).collect();
        let written = self.write(|obj| {
            let entry = obj.entry("secrets").or_insert_with(|| json!({}));
            if !entry.is_object() {
                *entry = json!({});
            }
            for (name, text) in &sealed {
                entry[*name] = json!(text);
            }
        });
        if let Err(e) = written {
            tracing::warn!("Couldn't move the saved {names} into the config: {e:#}");
            return;
        }
        for (name, _) in &found {
            if let Err(e) = legacy.delete(name) {
                tracing::warn!("Moved {name} into the config but couldn't delete the old entry: {e:#}");
            }
        }
        tracing::info!("Moved the saved {names} from the old credential store into the encrypted config.");
    }
}

/// The saved `name`: `None` when the config has no entry, else its value or `Unreadable` (no key, the wrong key, a
/// changed ciphertext, another name's).
fn read_saved(
    name: &str,
    stored: &BTreeMap<String, String>,
    key: &Result<Option<DataKey>, String>,
) -> Option<Result<Zeroizing<String>, Unreadable>> {
    let text = stored.get(name)?;
    Some(match key {
        Ok(Some(key)) => open(key, name, text).map(Zeroizing::new),
        _ => Err(Unreadable),
    })
}

#[cfg(test)]
mod custom_tests;
#[cfg(test)]
mod program_tests;
#[cfg(test)]
mod tests;
