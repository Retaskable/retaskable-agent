//! What `hello` says about this computer's settings (agent-settings.md sections 2.1 and 3.2): the owner's settings, the
//! computer-only values the settings page shows, and each environment variable's name with whether it is set, never
//! its value. Also the signal that sends every project's hello again after the settings page saves.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Map, Value, json};
use tokio::sync::watch;

use crate::config;
use crate::env::Env;
use crate::settings_server::engine_choice_json;
use crate::store::MachineStore;
use crate::tags;

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
}

#[derive(Clone)]
struct Source {
    dir: PathBuf,
    env: Env,
    default_port: u16,
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
    pub fn new(dir: PathBuf, env: Env, default_port: u16) -> Self {
        Self {
            source: Some(Source { dir, env, default_port }),
            ..Self::default()
        }
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

    /// The hello's `settings`: `None` when nothing says where to read them.
    pub async fn json(&self) -> Option<Value> {
        let s = self.source.as_ref()?;
        let (mut out, ci_slots) = match config::load(&s.dir) {
            Ok(cfg) => (from_config(&cfg, s), Some(config::ci_slots(&cfg, &s.env))),
            Err(_) => (Map::new(), None),
        };
        if let Some(n) = ci_slots {
            out.insert("ciSlots".into(), json!(n));
        }
        out.insert("envVars".into(), env_vars_json(&s.env));
        Some(Value::Object(out))
    }
}

fn from_config(cfg: &config::ConfigFile, s: &Source) -> Map<String, Value> {
    let m = &cfg.machine;
    let env = &s.env;
    let machine = MachineStore::load(&s.dir).ok();
    let found = tags::found_tags(&tags::machine_facts(env));
    let overrides = machine.as_ref().and_then(|m| m.tags());
    let local = config::local_model(cfg, env);
    let cursor = config::cursor_models(cfg, env);
    let network = env.get("RETASKABLE_NETWORK_PASSWORD").is_some()
        || machine.as_ref().and_then(|m| m.network()).is_some_and(|n| n.enabled);
    let Value::Object(out) = json!({
        "dailyUsd": m.budget.daily_usd,
        "concurrency": m.concurrency,
        "modelConcurrency": { "opus": m.model_concurrency.opus, "sonnet": m.model_concurrency.sonnet },
        "models": {
            "opusAbovePct": m.models.opus_above_pct,
            "sonnetAbovePct": m.models.sonnet_above_pct,
            "pauseBelowPct": m.models.pause_below_pct,
        },
        "port": config::port(cfg, env, s.default_port),
        "lanes": config::lanes(cfg, env),
        "pollIntervalSec": config::poll_interval_sec(cfg, env),
        "local": { "enabled": local.enabled, "model": local.model, "baseUrl": local.base_url },
        "engines": engine_choice_json(&config::engine_choice(cfg, env)),
        "cursorModels": { "top": cursor.top, "implementer": cursor.implementer, "scout": cursor.scout },
        "tags": tags::effective_tags(&found, overrides.as_ref(), env),
        "networkAccess": network,
    }) else {
        unreachable!()
    };
    out
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

    #[tokio::test]
    async fn without_a_source_nothing_is_read() {
        let h = HelloSettings::default();
        assert!(h.dir().is_none());
        assert!(h.json().await.is_none());
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
