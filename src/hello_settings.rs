//! What `hello` says about this computer's settings (agent-settings.md sections 2.1 and 3.2): the owner's settings, the
//! computer-only values the settings page shows, and each environment variable's name with whether it is set, never
//! its value. Also the signal that sends every project's hello again after the settings page saves.
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use serde_json::{Value, json};
use tokio::sync::watch;

use crate::config;
use crate::env::Env;
use crate::secrets::{Secrets, Source as SecretSource};
use crate::store::MachineStore;

/// agent-settings.md section 3.2: the environment variables that set a computer-only setting. The settings page greys
/// each one out and the hello lists it as set or not.
pub const AGENT_ENV_VARS: [&str; 10] = [
    "RETASKABLE_PORT",
    "RETASKABLE_LANES",
    "RETASKABLE_CI_SLOTS",
    "RETASKABLE_POLL_SEC",
    "RETASKABLE_TAGS",
    "RETASKABLE_LOCAL_MODEL",
    "RETASKABLE_LOCAL_URL",
    "RETASKABLE_ENGINES",
    "RETASKABLE_CURSOR_MODELS",
    "RETASKABLE_NETWORK_PASSWORD",
];

/// Each variable's name and whether it is set.
pub fn env_vars_json(env: &Env) -> Value {
    Value::Object(
        AGENT_ENV_VARS
            .iter()
            .map(|name| ((*name).to_string(), Value::Bool(env.get(name).is_some())))
            .collect(),
    )
}

/// Where the hello reads the settings from, and the signal that a save fired. Clones share the signal.
#[derive(Clone)]
pub struct HelloSettings {
    saved: Arc<watch::Sender<u64>>,
    source: Option<Source>,
    /// The saved secrets, for their names and source in the hello (given once at start; clones share it).
    secrets: Arc<OnceLock<Arc<Secrets>>>,
}

#[derive(Clone)]
struct Source {
    dir: PathBuf,
    env: Env,
}

impl std::fmt::Debug for HelloSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HelloSettings(..)")
    }
}

impl Default for HelloSettings {
    fn default() -> Self {
        Self {
            saved: Arc::new(watch::Sender::new(0)),
            source: None,
            secrets: Arc::default(),
        }
    }
}

impl PartialEq for HelloSettings {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl HelloSettings {
    /// Reads the config in `dir` with `env` over it, as the settings page does.
    pub fn new(dir: PathBuf, env: Env) -> Self {
        Self {
            source: Some(Source { dir, env }),
            ..Self::default()
        }
    }

    /// Has the hello say where each secret comes from (agent-settings.md section 3.3), by name only.
    pub fn set_secrets(&self, secrets: Arc<Secrets>) {
        let _ = self.secrets.set(secrets);
    }

    /// Makes every open connection send a fresh hello.
    pub fn fire(&self) {
        self.saved.send_modify(|n| *n += 1);
    }

    /// What `project` has cost today on this computer, and the machine's `budget.dailyUsd` (0 when unset).
    pub fn spend_today(&self, project: &str) -> (f64, f64) {
        let Some(dir) = self.dir() else {
            return (0.0, 0.0);
        };
        let spent = MachineStore::load(dir).map_or(0.0, |m| m.spend_today(project));
        let daily = config::load(dir).map_or(0.0, |c| c.machine.budget.daily_usd);
        (spent, daily)
    }

    /// The directory holding `config.json`, when something says where it is.
    pub fn dir(&self) -> Option<&Path> {
        self.source.as_ref().map(|s| s.dir.as_path())
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.saved.subscribe()
    }

    /// The hello's `computer` (shared/cloud.ts `computerHello`): `{ lanes, secrets: [{ key, state, kind }] }`, each
    /// secret by name only, listed when it is set (`state: "set"`, `kind: "env"` when the environment sets it, `"key"`
    /// when it is saved here), as the TypeScript agent does. Never a value, a fingerprint or ciphertext. `None` when
    /// nothing says where the config is, or it won't read: the site drops a malformed `computer` and wants `lanes`.
    pub fn computer(&self) -> Option<Value> {
        let s = self.source.as_ref()?;
        let cfg = config::load(&s.dir).ok()?;
        let mut secrets: Vec<Value> = AGENT_ENV_VARS
            .iter()
            .filter(|name| s.env.get(name).is_some())
            .map(|name| json!({ "key": name, "state": "set", "kind": "env" }))
            .collect();
        if let Some(saved) = self.secrets.get() {
            for t in saved.states() {
                let kind = match t.source {
                    SecretSource::Env => "env",
                    SecretSource::Saved => "key",
                    SecretSource::Unset | SecretSource::Unreadable => continue,
                };
                // The site learns the known secrets only, never the names of variables a person added.
                if t.def.is_some() {
                    secrets.push(json!({ "key": t.name, "state": "set", "kind": kind }));
                }
            }
        }
        Some(json!({ "lanes": config::lanes(&cfg, &s.env), "secrets": secrets }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_vars_json_lists_every_name_with_whether_it_is_set_never_the_value() {
        let env = Env::from_pairs([("RETASKABLE_PORT", "4567"), ("RETASKABLE_NETWORK_PASSWORD", "hunter2")]);
        let v = env_vars_json(&env);
        let obj = v.as_object().unwrap();
        assert_eq!(obj.len(), AGENT_ENV_VARS.len());
        for name in AGENT_ENV_VARS {
            let set = name == "RETASKABLE_PORT" || name == "RETASKABLE_NETWORK_PASSWORD";
            assert_eq!(obj[name], Value::Bool(set), "{name}");
        }
        assert!(!v.to_string().contains("hunter2"));
    }

    #[test]
    fn env_vars_json_is_all_false_for_an_empty_env() {
        let v = env_vars_json(&Env::empty());
        assert!(v.as_object().unwrap().values().all(|b| *b == Value::Bool(false)));
    }

    /// shared/cloud.ts `computerHello`: `lanes` a number, `secrets` at most 60 of `{ key (<= 300), state (<= 20) }`
    /// (loose, so `kind` rides along). A `computer` that doesn't fit is dropped silently by the site.
    #[tokio::test]
    async fn the_hello_computer_is_lanes_and_secrets_by_name_and_state_only() {
        use crate::secrets::fakes::{FakeHost, MemStore};
        const PLANTED: &str = "ghp_PLANTED_hello_value_42";
        let dir = tempfile::tempdir().unwrap();
        let host = Arc::new(FakeHost::with(&[("GITLAB_TOKEN", "glpat-from-env")]));
        let secrets = Secrets::new(
            dir.path(),
            &Env::empty(),
            host.clone(),
            Some(Arc::new(MemStore::default())),
            Arc::new(MemStore::default()),
        );
        secrets.apply();
        let plan = secrets
            .plan(
                json!({ "GH_TOKEN": PLANTED, "ANTHROPIC_API_KEY": null })
                    .as_object()
                    .unwrap(),
            )
            .unwrap();
        secrets.commit(plan).unwrap();
        let env = Env::from_pairs([("RETASKABLE_NETWORK_PASSWORD", "hunter2")]);
        let h = HelloSettings::new(dir.path().to_path_buf(), env);
        h.set_secrets(Arc::new(secrets));

        let computer = h.computer().unwrap();
        let top: Vec<&String> = computer.as_object().unwrap().keys().collect();
        assert_eq!(top, ["lanes", "secrets"]);
        assert!(computer["lanes"].is_number());
        let rows = computer["secrets"].as_array().unwrap();
        assert!(rows.len() <= 60);
        let mut seen = Vec::new();
        for row in rows {
            let keys: Vec<&String> = row.as_object().unwrap().keys().collect();
            assert_eq!(keys, ["key", "kind", "state"], "{row}");
            assert!(row["key"].as_str().unwrap().len() <= 300);
            assert_eq!(row["state"], "set");
            seen.push((row["key"].as_str().unwrap(), row["kind"].as_str().unwrap()));
        }
        assert_eq!(
            seen,
            [
                ("RETASKABLE_NETWORK_PASSWORD", "env"),
                ("GH_TOKEN", "key"),
                ("GITLAB_TOKEN", "env"),
            ]
        );
        let text = computer.to_string();
        for never in [
            PLANTED,
            "glpat-from-env",
            "hunter2",
            &crate::secrets::fingerprint(PLANTED),
        ] {
            assert!(!text.contains(never), "{never} in {text}");
        }
        assert!(HelloSettings::default().computer().is_none());
    }

    #[tokio::test]
    async fn without_a_source_nothing_is_read() {
        let h = HelloSettings::default();
        assert!(h.dir().is_none());
        assert_eq!(h.spend_today("any"), (0.0, 0.0));
    }

    #[test]
    fn fire_wakes_subscribers_of_every_clone() {
        let h = HelloSettings::default();
        let clone = h.clone();
        let mut rx = h.subscribe();
        assert!(!rx.has_changed().unwrap());
        clone.fire();
        assert!(rx.has_changed().unwrap());
        assert_eq!(*rx.borrow_and_update(), 1);
        h.fire();
        assert_eq!(*rx.borrow_and_update(), 2);
    }

    #[test]
    fn all_hello_settings_compare_equal() {
        assert_eq!(HelloSettings::default(), HelloSettings::default());
    }
}
