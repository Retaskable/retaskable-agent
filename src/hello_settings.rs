//! What `hello` says about this computer's settings (agent-settings.md sections 2.1 and 3.2): the owner's settings, the
//! computer-only values the settings page shows, and each environment variable's name with whether it is set, never
//! its value. Also the signal that sends every project's hello again after the settings page saves.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Map, Value, json};
use tokio::sync::watch;

use crate::config;
use crate::env::Env;
use crate::flags::Flags;
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

/// The release flag that shows the settings in the hello; Off leaves the hello as it was.
pub const FLAG: &str = "machine-settings";

/// The release flag that shows the CI slots setting (agent-ci.md); Off leaves the hello and the page as they were.
pub const CI_FLAG: &str = "agent-ci";

/// The release flag that sends the setup checks in the hello (first-run-check.md section 3.1); Off leaves the hello as it was.
/// The flag client's `PROJECT_ID` and `RETASKABLE_FLAG_KEY` are placeholders the owner fills in.
pub const CHECKS_FLAG: &str = "setup-check-command";

#[derive(Clone)]
struct Source {
    dir: PathBuf,
    env: Env,
    default_port: u16,
    flags: Arc<Flags>,
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
    pub fn new(dir: PathBuf, env: Env, default_port: u16, flags: Arc<Flags>) -> Self {
        Self {
            source: Some(Source {
                dir,
                env,
                default_port,
                flags,
            }),
            ..Self::default()
        }
    }

    /// Makes every open connection send a fresh hello.
    pub fn fire(&self) {
        self.saved.send_modify(|n| *n += 1);
    }

    /// Whether the `machine-settings` flag is On, asking again when the last answer is stale; no source means Off.
    pub async fn on(&self) -> bool {
        match &self.source {
            Some(s) => s.flags.on(FLAG).await,
            None => false,
        }
    }

    /// Whether the `setup-check-command` flag is On, asking again when the last answer is stale; no source means Off.
    pub async fn setup_checks_on(&self) -> bool {
        match &self.source {
            Some(s) => s.flags.on(CHECKS_FLAG).await,
            None => false,
        }
    }

    /// The same from the last answer, without asking (for code that isn't async); Off until there is one.
    pub fn on_now(&self) -> bool {
        self.source.as_ref().is_some_and(|s| s.flags.last(FLAG))
    }

    /// Whether the `snapshot-reporting` flag was On at the last answer, without asking; Off until there is one.
    pub fn snapshot_reporting_on(&self) -> bool {
        self.source
            .as_ref()
            .is_some_and(|s| s.flags.last(crate::view::SNAPSHOT_REPORTING_FLAG))
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

    /// The hello's `settings`: `None` while the flag is Off or when nothing says where to read them.
    pub async fn json(&self) -> Option<Value> {
        let s = self.source.as_ref()?;
        if !s.flags.on(FLAG).await {
            return None;
        }
        let (mut out, ci_slots) = match config::load(&s.dir) {
            Ok(cfg) => (from_config(&cfg, s), Some(config::ci_slots(&cfg, &s.env))),
            Err(_) => (Map::new(), None),
        };
        if let Some(n) = ci_slots
            && s.flags.on(CI_FLAG).await
        {
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
        "autoConcurrency": { "enabled": m.auto_concurrency.enabled, "min": m.auto_concurrency.min, "max": m.auto_concurrency.max,
            "targetPct": m.auto_concurrency.target_pct, "weekMarginPct": m.auto_concurrency.week_margin_pct, "weekFloor": m.auto_concurrency.week_floor },
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
