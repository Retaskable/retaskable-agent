//! `hello.computer` (`ComputerView` in `shared/computerView.ts`, built by `agent/src/computerView.ts`): every
//! machine-level setting but file paths, for Settings -> Agents. The secrets ride beside it ([`crate::hello_settings`]);
//! a value is never read into any of it. `local.server` is the configured address even while the model is off (the
//! TypeScript agent says "" with no `local` at all).
//!
//! Not sent, as the TypeScript view's optional fields: `lanesAuto` (the lane count doesn't follow the Claude plan
//! here) and `usesEngines` (there is no `work` setting to read it from), each "none from an older agent". `releases`
//! is per project, so [`crate::hello_settings::HelloSettings::computer`] adds it from the release gate.
use std::sync::Arc;

use serde_json::{Map, Value, json};

use crate::config::{self, ConfigFile, Engine};
use crate::env::Env;
use crate::link::pending::Links;
use crate::link::{self, TokenStore};
use crate::network;
use crate::release::Build;
use crate::store::MachineStore;

/// What the view needs beyond `config.json` and the environment, given once at start: where the agent listens by
/// default, the build, and the link states of the clouds and the backlog store.
#[derive(Clone)]
pub struct ComputerContext {
    pub default_port: u16,
    pub build: Option<Build>,
    pub keychain: Arc<dyn TokenStore>,
    pub links: Links,
}

/// An address as its origin, or its host and port, with nothing else in it (no user, password, path or query):
/// `bareAddress`. An address that isn't one reads as "".
pub fn bare_address(url: &str, origin: bool) -> String {
    let Ok(u) = reqwest::Url::parse(url) else {
        return String::new();
    };
    if origin {
        return u.origin().ascii_serialization();
    }
    match (u.host_str(), u.port()) {
        (Some(h), Some(p)) => format!("{h}:{p}"),
        (Some(h), None) => h.to_string(),
        _ => String::new(),
    }
}

fn engine(e: Option<Engine>, none: &str) -> String {
    e.map_or_else(|| none.to_string(), |e| e.to_string())
}

/// The links, by name and state (`computerLinks`): each cloud by its host, then the backlog store.
pub fn links(ctx: &ComputerContext, cfg: &ConfigFile, env: &Env) -> Vec<Value> {
    let word = |url: &str, waiting: bool| {
        if waiting {
            "waiting"
        } else if matches!(ctx.keychain.get(url), Ok(Some(_))) {
            "linked"
        } else {
            "not linked"
        }
    };
    let host = |url: &str| {
        reqwest::Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
    };
    let mut rows: Vec<Value> = link::cloud_urls(cfg, env, ctx.build.as_ref())
        .iter()
        .filter_map(|url| {
            let state = word(url, ctx.links.waiting(url).is_some());
            Some(json!({ "key": host(url)?, "state": state, "kind": "link" }))
        })
        .collect();
    let backlog = crate::backlog::url(env);
    if let Some(h) = host(&backlog) {
        let state = word(&backlog, ctx.links.backlog().is_some());
        rows.push(json!({ "key": format!("backlog store {h}"), "state": state, "kind": "link" }));
    }
    rows
}

/// What the store's two tiers mean on this computer (`tiersOf` in `agent/src/computerView.ts`; pools-on-server.md section 7, decided
/// 2026-11-17: the store names a tier only, and this computer picks the engine and model from its own settings): the engine the
/// implementer's work runs on first and its model for `smart` and `fast`. Claude: Opus and Sonnet; Cursor: its top and implementer
/// models; any other engine picks its own, so its model is "". The local model, when on, is a floor while Claude is at its limit, not a tier.
/// Reported, not chosen: the map is the agent's own (`tiers.rs`, `engine_choice`).
pub fn tiers(choice: &config::EngineChoiceConfig, cursor: &config::CursorModels) -> Value {
    let first = choice.roles.implementer.or(choice.default).unwrap_or(Engine::Claude);
    let (smart, fast) = match first {
        Engine::Claude => ("opus".to_string(), "sonnet".to_string()),
        Engine::Cursor => (
            cursor.top.clone().unwrap_or_default(),
            cursor.implementer.clone().unwrap_or_default(),
        ),
        _ => (String::new(), String::new()),
    };
    let one = |model: String| json!({ "engine": first.to_string(), "model": model });
    json!({ "smart": one(smart), "fast": one(fast) })
}

/// Everything in the view but `secrets`. `found` is the tags this computer found for itself.
pub fn view(
    cfg: &ConfigFile,
    env: &Env,
    machine: Option<&MachineStore>,
    ctx: Option<&ComputerContext>,
    found: &[String],
) -> Map<String, Value> {
    let m = &cfg.machine;
    let mut out = Map::new();
    let concurrency = m.concurrency;
    out.insert("lanes".into(), json!(config::lanes(cfg, env)));
    if let Some(s) = &m.lane_scaling {
        out.insert(
            "laneScaling".into(),
            json!({ "base": s.base, "belowUsedPct": s.below_used_pct }),
        );
    }
    if let Some(c) = m.chore_lane {
        out.insert("choreLane".into(), json!(c));
    }
    out.insert("concurrency".into(), json!(concurrency));
    let caps = &m.model_concurrency;
    if caps.opus.is_some() || caps.sonnet.is_some() {
        // A model with no cap of its own takes `concurrency` (configTypes.ts `modelConcurrency`).
        out.insert(
            "modelConcurrency".into(),
            json!({ "opus": caps.opus.unwrap_or(concurrency), "sonnet": caps.sonnet.unwrap_or(concurrency) }),
        );
    }
    out.insert("pollIntervalSec".into(), json!(config::poll_interval_sec(cfg, env)));
    let default_port = ctx.map_or(config::DEFAULT_PORT, |c| c.default_port);
    out.insert("port".into(), json!(config::port(cfg, env, default_port)));
    out.insert(
        "models".into(),
        json!({
            "opusAbovePct": m.models.opus_above_pct,
            "sonnetAbovePct": m.models.sonnet_above_pct,
            "pauseBelowPct": m.models.pause_below_pct,
        }),
    );
    let local = config::local_model(cfg, env);
    out.insert(
        "local".into(),
        json!({ "enabled": local.enabled, "model": local.model, "server": bare_address(&local.base_url, false) }),
    );
    out.insert("dailyUsd".into(), json!(m.budget.daily_usd));
    let cursor = config::cursor_models(cfg, env);
    out.insert(
        "cursorModels".into(),
        json!({
            "top": cursor.top.clone().unwrap_or_default(),
            "implementer": cursor.implementer.clone().unwrap_or_default(),
            "scout": cursor.scout.clone().unwrap_or_default(),
        }),
    );
    let choice = config::engine_choice(cfg, env);
    out.insert("tiers".into(), tiers(&choice, &cursor));
    let r = &choice.roles;
    out.insert(
        "engines".into(),
        json!({
            "default": engine(choice.default, "claude"),
            "roles": {
                "planner": engine(r.planner, "default"),
                "implementer": engine(r.implementer, "default"),
                "review": engine(r.review, "default"),
                "drafter": engine(r.drafter, "default"),
                "chat": engine(r.chat, "default"),
                "writer": engine(r.writer, "default"),
            },
        }),
    );
    let cloud = link::cloud_url(cfg, env, ctx.and_then(|c| c.build.as_ref())).unwrap_or_default();
    out.insert("cloud".into(), json!(bare_address(&cloud, true)));
    out.insert("backlog".into(), json!(bare_address(&crate::backlog::url(env), true)));
    let tags = machine.and_then(MachineStore::tags).unwrap_or_default();
    out.insert(
        "tags".into(),
        json!({ "found": found, "added": tags.added, "removed": tags.removed }),
    );
    let on = network::env_password_set(env) || machine.and_then(MachineStore::network).is_some_and(|n| n.enabled);
    out.insert("network".into(), json!(on));
    out
}

#[cfg(test)]
mod tests;
