//! The tokens the agent's tools read from the environment (agent-settings.md section 3.3): GitHub's, Claude Code's,
//! GitLab's and Cloudflare's. An environment variable wins; otherwise one saved on the settings page is put in this
//! process's environment, which every session, check and script it starts inherits. A saved one lives in
//! `config.json`, under `secrets`, as ciphertext (`cipher.rs`) under a key kept apart from the config (`key.rs`).
//! The page shows each as set or not and a short fingerprint, never the value; the site gets names and source only.
//! A value that can't be opened is "unreadable", never plaintext, never a crash.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::config;
use crate::env::Env;
use crate::link::{self, TokenStore};
use crate::sync::lock;

mod cipher;
#[cfg(test)]
pub(crate) mod fakes;
mod host;
mod key;

pub use cipher::{DataKey, Unreadable, open, seal};
pub use host::{Host, ProcessHost};
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
    pub def: &'static SecretDef,
    pub source: Source,
    /// The first 8 hex characters of the value's SHA-256, to tell which one is in use. Never sent to the site.
    pub fingerprint: Option<String>,
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
    items: Vec<(&'static str, Option<Applied>)>,
}

struct Applied {
    value: String,
    sealed: String,
}

impl Plan {
    pub fn names(&self) -> Vec<&'static str> {
        self.items.iter().map(|(name, _)| *name).collect()
    }
}

#[derive(Default)]
struct Inner {
    /// The names the environment set when the agent started.
    from_env: BTreeSet<String>,
    /// The names this agent put in the environment from the saved ones.
    saved: BTreeSet<String>,
}

pub struct Secrets {
    dir: PathBuf,
    keys: Keys,
    host: Arc<dyn Host>,
    /// The TypeScript agent's store, for the move at start.
    legacy: Arc<dyn TokenStore>,
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
        Self {
            dir: dir.to_path_buf(),
            keys: Keys::open(dir, env, os),
            host,
            legacy,
            inner: Mutex::new(Inner::default()),
        }
    }

    /// Why a save can't work on this computer (no key to seal with), or `None`.
    pub fn save_problem(&self) -> Option<String> {
        self.keys.save_problem()
    }

    /// At start, before anything is spawned: moves the TypeScript agent's saved secrets in, notes which names the
    /// environment set (an empty value is none), and puts the saved ones in the environment for the rest.
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
        inner.from_env.clear();
        inner.saved.clear();
        for def in &SECRETS {
            let saved = read_saved(def.name, &stored, &key);
            let env_value = self
                .host
                .get(def.name)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty());
            // Put here from the saved one by the agent before a restart (and unchanged since): still the saved one.
            if let Some(value) = &env_value
                && !(before.contains(def.name) && matches!(&saved, Some(Ok(plain)) if plain == value))
            {
                inner.from_env.insert(def.name.to_string());
                continue;
            }
            match saved {
                Some(Ok(plain)) => {
                    self.host.set(def.name, &plain);
                    inner.saved.insert(def.name.to_string());
                }
                Some(Err(Unreadable)) => {
                    tracing::warn!(
                        "The saved {} can't be read with this computer's key: save it again.",
                        def.name
                    );
                    self.host.remove(def.name);
                }
                None => self.host.remove(def.name),
            }
        }
        self.mark_saved(&inner);
        if inner.saved.contains("GH_TOKEN") {
            self.host.git_login();
        }
    }

    pub fn states(&self) -> Vec<SecretState> {
        let stored = self.stored();
        let key = self.keys.for_reading();
        let inner = lock(&self.inner);
        SECRETS
            .iter()
            .map(|def| {
                let (source, value) = if inner.from_env.contains(def.name) {
                    (Source::Env, self.host.get(def.name))
                } else {
                    match read_saved(def.name, &stored, &key) {
                        Some(Ok(plain)) => (Source::Saved, Some(plain)),
                        Some(Err(Unreadable)) => (Source::Unreadable, None),
                        None => (Source::Unset, None),
                    }
                };
                SecretState {
                    def,
                    source,
                    fingerprint: value.map(|v| fingerprint(&v)),
                }
            })
            .collect()
    }

    /// Checks every change (a value to save, or `null` to clear) and seals the values, writing nothing.
    pub fn plan(&self, changes: &Map<String, Value>) -> Result<Plan, Refused> {
        let inner = lock(&self.inner);
        let mut items = Vec::new();
        for (name, raw) in changes {
            let Some(def) = SECRETS.iter().find(|d| d.name == name) else {
                // Not echoed: a request's own text is never put in a reply.
                return Err(Refused("That isn't a secret this page sets.".to_string()));
            };
            if inner.from_env.contains(def.name) {
                return Err(Refused(format!("{name} is set by the environment: change it there.")));
            }
            let value = match raw {
                Value::Null => {
                    items.push((def.name, None));
                    continue;
                }
                Value::String(text) => text.trim(),
                _ => return Err(Refused(format!("{name} must be text."))),
            };
            if value.is_empty() || value.chars().count() > MAX_LEN {
                return Err(Refused(format!("{name} must be 1 to {MAX_LEN} characters.")));
            }
            if value.chars().any(|c| c.is_whitespace() || c.is_control()) {
                return Err(Refused(format!("{name} can't hold spaces or line breaks.")));
            }
            items.push((def.name, Some(value.to_string())));
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
                    sealed: seal(key, name, &value),
                    value,
                });
                (name, applied)
            })
            .collect();
        Ok(Plan { items })
    }

    /// Writes a checked plan: into the config, then the environment new sessions inherit. Sessions already running keep
    /// the environment they started with.
    pub fn commit(&self, plan: Plan) -> Result<()> {
        if plan.items.is_empty() {
            return Ok(());
        }
        let mut inner = lock(&self.inner);
        config::save_machine_settings(&self.dir, |obj| {
            let entry = obj.entry("secrets").or_insert_with(|| json!({}));
            if !entry.is_object() {
                *entry = json!({});
            }
            let map = entry.as_object_mut().expect("made an object above");
            for (name, applied) in &plan.items {
                match applied {
                    Some(a) => map.insert((*name).to_string(), json!(a.sealed)),
                    None => map.remove(*name),
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
                    self.host.set(name, &a.value);
                    inner.saved.insert((*name).to_string());
                    gh_changed |= *name == "GH_TOKEN";
                }
                None => {
                    self.host.remove(name);
                    inner.saved.remove(*name);
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
        let text = std::fs::read_to_string(self.dir.join("config.json")).unwrap_or_default();
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
        let stored = self.stored();
        let mut found = Vec::new();
        for def in SECRETS.iter().filter(|d| !stored.contains_key(d.name)) {
            match self.legacy.get(def.name) {
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
        let written = config::save_machine_settings(&self.dir, |obj| {
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
            if let Err(e) = self.legacy.delete(name) {
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
) -> Option<Result<String, Unreadable>> {
    let text = stored.get(name)?;
    Some(match key {
        Ok(Some(key)) => open(key, name, text),
        _ => Err(Unreadable),
    })
}

#[cfg(test)]
mod tests;
