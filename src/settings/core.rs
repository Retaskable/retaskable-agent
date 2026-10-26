//! What the settings page reads and saves (agent-settings.md section 3; TypeScript's
//! `machineSettings.ts`): the computer-only settings' current values, each one's environment
//! lock, and a checked, applied change. Pure and synchronous, so it's tested without a server.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::config::{self, ConfigFile};
use crate::network::{self, NetworkAccess};
use crate::password;
use crate::tags::{self, TagOverrides};

const ENGINE_PICKS: [&str; 4] = ["claude", "cursor", "pi", "local"];
const MAX_LANES: u32 = 64;

fn valid_engine_pick(s: &str, allow_default: bool) -> bool {
    ENGINE_PICKS.contains(&s) || (allow_default && s == "default")
}

/// "The local model's name is up to 100 letters, digits and . _ : / -.": unlike the environment's
/// parser, empty is allowed here (it means off), the same as `RETASKABLE_CURSOR_MODELS`'.
fn valid_model_name_allow_empty(s: &str) -> bool {
    s.is_empty()
        || (s.chars().count() <= 100
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '/' | '-')))
}

/// Which setting: matches `SettingKey` in the TypeScript agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SettingKey {
    Port,
    Lanes,
    Local,
    LocalUrl,
    PollIntervalSec,
    Tags,
    CursorModels,
    Engines,
    Network,
}

impl SettingKey {
    fn as_str(self) -> &'static str {
        match self {
            SettingKey::Port => "port",
            SettingKey::Lanes => "lanes",
            SettingKey::Local => "local",
            SettingKey::LocalUrl => "localUrl",
            SettingKey::PollIntervalSec => "pollIntervalSec",
            SettingKey::Tags => "tags",
            SettingKey::CursorModels => "cursorModels",
            SettingKey::Engines => "engines",
            SettingKey::Network => "network",
        }
    }

    fn env_var(self) -> &'static str {
        match self {
            SettingKey::Port => "RETASKABLE_PORT",
            SettingKey::Lanes => "RETASKABLE_LANES",
            SettingKey::Local => "RETASKABLE_LOCAL_MODEL",
            SettingKey::LocalUrl => "RETASKABLE_LOCAL_URL",
            SettingKey::PollIntervalSec => "RETASKABLE_POLL_SEC",
            SettingKey::Tags => "RETASKABLE_TAGS",
            SettingKey::CursorModels => "RETASKABLE_CURSOR_MODELS",
            SettingKey::Engines => "RETASKABLE_ENGINES",
            SettingKey::Network => "RETASKABLE_NETWORK_PASSWORD",
        }
    }

    fn label(self) -> &'static str {
        match self {
            SettingKey::Port => "The port",
            SettingKey::Lanes => "The lanes",
            SettingKey::Local => "The local model",
            SettingKey::LocalUrl => "The local model's address",
            SettingKey::PollIntervalSec => "How often it checks",
            SettingKey::Tags => "Tags",
            SettingKey::CursorModels => "Cursor's models",
            SettingKey::Engines => "The engines",
            SettingKey::Network => "Network access",
        }
    }
}

/// Which environment variable set which setting, by `SettingKey::as_str`, for the page's
/// `setBy` and the JSON response's key.
pub type SetBy = BTreeMap<&'static str, String>;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalView {
    pub enabled: bool,
    pub model: String,
    pub base_url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorModelsView {
    pub top: String,
    pub implementer: String,
    pub scout: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RolesView {
    pub planner: String,
    pub implementer: String,
    pub review: String,
    pub drafter: String,
    pub chat: String,
    pub writer: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnginesView {
    pub default: String,
    pub roles: RolesView,
    pub lanes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkView {
    pub enabled: bool,
    pub password_set: bool,
}

/// What the page shows (`MachineSettings`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineSettingsView {
    pub port: u16,
    pub lanes: u32,
    pub local: LocalView,
    pub poll_interval_sec: u32,
    pub tags: Vec<String>,
    pub cursor_models: CursorModelsView,
    pub engines: EnginesView,
    pub network: NetworkView,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalChange {
    pub enabled: Option<bool>,
    pub model: Option<String>,
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorModelsChange {
    pub top: Option<String>,
    pub implementer: Option<String>,
    pub scout: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RolesChange {
    pub planner: Option<String>,
    pub implementer: Option<String>,
    pub review: Option<String>,
    pub drafter: Option<String>,
    pub chat: Option<String>,
    pub writer: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnginesChange {
    pub default: Option<String>,
    pub roles: Option<RolesChange>,
    pub lanes: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkChange {
    pub enabled: Option<bool>,
    pub password: Option<String>,
}

/// A change to the settings (`SettingsChange`): any part left out keeps its current value.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsChange {
    pub port: Option<u16>,
    pub lanes: Option<u32>,
    pub local: Option<LocalChange>,
    pub poll_interval_sec: Option<u32>,
    pub tags: Option<Vec<String>>,
    pub cursor_models: Option<CursorModelsChange>,
    pub engines: Option<EnginesChange>,
    pub network: Option<NetworkChange>,
}

/// The settings as they are: the config file's and machine.json's, with the environment's
/// values over them, and which environment variable set which.
pub fn current_settings(
    cfg: &ConfigFile,
    tags_overrides: Option<&TagOverrides>,
    network_stored: Option<&NetworkAccess>,
    found: &[String],
    env: impl Fn(&str) -> Option<String> + Copy,
) -> (MachineSettingsView, SetBy) {
    let lanes = config::lanes(cfg, env);
    let local = config::local_model(cfg, env);
    let cursor_models = config::cursor_models(cfg, env);
    let network_access = network::network_access(network_stored, env);
    let view = MachineSettingsView {
        port: config::port(cfg, env),
        lanes,
        local: LocalView {
            enabled: local.enabled,
            model: local.model,
            base_url: local.base_url,
        },
        poll_interval_sec: config::poll_interval_sec(cfg, env),
        tags: tags::effective_tags(found, tags_overrides, env),
        cursor_models: CursorModelsView {
            top: cursor_models.top.unwrap_or_default(),
            implementer: cursor_models.implementer.unwrap_or_default(),
            scout: cursor_models.scout.unwrap_or_default(),
        },
        engines: engines_view(cfg, env, lanes),
        network: NetworkView {
            enabled: network_access.enabled,
            password_set: network_access.password_hash.is_some(),
        },
    };
    (view, set_by(env))
}

fn engines_view(cfg: &ConfigFile, env: impl Fn(&str) -> Option<String> + Copy, lanes: u32) -> EnginesView {
    let choice = config::engine_choice(cfg, env);
    let role = |e: Option<config::Engine>| e.map(|e| e.to_string()).unwrap_or_else(|| "default".to_string());
    EnginesView {
        default: choice.default.unwrap_or(config::Engine::Claude).to_string(),
        roles: RolesView {
            planner: role(choice.roles.planner),
            implementer: role(choice.roles.implementer),
            review: role(choice.roles.review),
            drafter: role(choice.roles.drafter),
            chat: role(choice.roles.chat),
            writer: role(choice.roles.writer),
        },
        lanes: lane_choices(&choice.lanes, lanes),
    }
}

/// The lanes the page shows: the saved choices, padded with "default" to the computer's lanes.
fn lane_choices(saved: &[config::LaneEngine], lanes: u32) -> Vec<String> {
    let count = (lanes as usize).max(saved.len());
    (0..count)
        .map(|i| saved.get(i).copied().unwrap_or(config::LaneEngine::Default).to_string())
        .collect()
}

fn set_by(env: impl Fn(&str) -> Option<String> + Copy) -> SetBy {
    let mut out = SetBy::new();
    let mut add = |key: SettingKey, locked: bool| {
        if locked {
            out.insert(key.as_str(), key.env_var().to_string());
        }
    };
    add(SettingKey::Port, config::port_locked(env));
    add(SettingKey::Lanes, config::lanes_locked(env));
    add(SettingKey::Local, config::local_model_locked(env));
    add(SettingKey::LocalUrl, config::local_url_locked(env));
    add(SettingKey::PollIntervalSec, config::poll_interval_sec_locked(env));
    add(SettingKey::Tags, tags::tags_from_env(env).is_some());
    add(SettingKey::CursorModels, config::cursor_models_locked(env));
    add(SettingKey::Engines, config::engines_locked(env));
    add(SettingKey::Network, network::network_locked(env));
    out
}

fn locked_message(key: SettingKey, set_by: &SetBy) -> Option<String> {
    set_by
        .get(key.as_str())
        .map(|var| format!("{} is set by {var}: change it there.", key.label()))
}

/// A change, checked and ready to save: the config file's top-level keys to write, and
/// machine.json's tags and network access, plus whether it waits for a restart.
#[derive(Debug)]
pub struct Applied {
    /// `config.json` keys to set (`port`, `lanes`, `local`, …), each a full replacement value.
    pub config_changes: Vec<(&'static str, Value)>,
    pub tags: Option<TagOverrides>,
    pub network: Option<NetworkAccess>,
    pub restart: bool,
    pub changed: bool,
}

/// A change, checked and applied (`applySettings`). A setting an environment variable sets
/// can't change here. `Err` is the message the page shows.
pub fn apply_settings(
    change: &SettingsChange,
    cfg: &ConfigFile,
    tags_overrides: Option<&TagOverrides>,
    network_stored: Option<&NetworkAccess>,
    found: &[String],
    env: impl Fn(&str) -> Option<String> + Copy,
) -> Result<Applied, String> {
    let (was, set_by) = current_settings(cfg, tags_overrides, network_stored, found, env);
    let locked = |key: SettingKey| locked_message(key, &set_by);
    let mut config_changes = Vec::new();
    let mut restart = false;
    let mut changed = false;
    let mut tags_out = None;
    let mut network_out = None;

    if let Some(port) = change.port
        && port != was.port
    {
        if !(1024..=65535).contains(&port) {
            return Err("The port is a whole number from 1024 to 65535.".to_string());
        }
        if let Some(why) = locked(SettingKey::Port) {
            return Err(why);
        }
        config_changes.push(("port", Value::from(port)));
        restart = true;
        changed = true;
    }
    if let Some(lanes) = change.lanes
        && lanes != was.lanes
    {
        if !(1..=MAX_LANES).contains(&lanes) {
            return Err(format!("The lanes are a whole number from 1 to {MAX_LANES}."));
        }
        if let Some(why) = locked(SettingKey::Lanes) {
            return Err(why);
        }
        config_changes.push(("lanes", Value::from(lanes)));
        changed = true;
    }
    if let Some(poll) = change.poll_interval_sec
        && poll != was.poll_interval_sec
    {
        if !(2..=3600).contains(&poll) {
            return Err("How often it checks is from 2 to 3600 seconds.".to_string());
        }
        if let Some(why) = locked(SettingKey::PollIntervalSec) {
            return Err(why);
        }
        config_changes.push(("pollIntervalSec", Value::from(poll)));
        changed = true;
    }
    if let Some(local) = &change.local {
        let next_enabled = local.enabled.unwrap_or(was.local.enabled);
        let next_model = local.model.clone().unwrap_or_else(|| was.local.model.clone());
        let next_base_url = local.base_url.clone().unwrap_or_else(|| was.local.base_url.clone());
        if !valid_model_name_allow_empty(&next_model) || (next_enabled && next_model.is_empty()) {
            return Err("The local model's name is up to 100 letters, digits and . _ : / -.".to_string());
        }
        if !next_base_url.is_empty() && !config::valid_local_url(&next_base_url) {
            return Err("The local model's address is an http or https address.".to_string());
        }
        let model_changed = next_enabled != was.local.enabled || next_model != was.local.model;
        let url_changed = next_base_url != was.local.base_url;
        if model_changed && let Some(why) = locked(SettingKey::Local) {
            return Err(why);
        }
        if url_changed && let Some(why) = locked(SettingKey::LocalUrl) {
            return Err(why);
        }
        if model_changed || url_changed {
            // The file's own value for whichever half didn't change, never `was` (which may
            // carry an environment override): a save must never write a variable's value in.
            let raw = &cfg.machine.local;
            config_changes.push((
                "local",
                serde_json::json!({
                    "enabled": if model_changed { next_enabled } else { raw.enabled },
                    "model": if model_changed { &next_model } else { &raw.model },
                    "baseUrl": if url_changed { &next_base_url } else { &raw.base_url },
                }),
            ));
            changed = true;
        }
    }
    if let Some(cm) = &change.cursor_models {
        let next_top = cm.top.clone().unwrap_or_else(|| was.cursor_models.top.clone());
        let next_impl = cm
            .implementer
            .clone()
            .unwrap_or_else(|| was.cursor_models.implementer.clone());
        let next_scout = cm.scout.clone().unwrap_or_else(|| was.cursor_models.scout.clone());
        for model in [&next_top, &next_impl, &next_scout] {
            if !model.is_empty() && !config::valid_cursor_model_name(model) {
                return Err("A Cursor model's name is up to 100 letters, digits and . _ : / -.".to_string());
            }
        }
        if next_top != was.cursor_models.top
            || next_impl != was.cursor_models.implementer
            || next_scout != was.cursor_models.scout
        {
            if let Some(why) = locked(SettingKey::CursorModels) {
                return Err(why);
            }
            // Written by the environment-locked setting's own env var; by this point a lock
            // would already have returned above, so `next_*` here is never an env value.
            let mut models = Map::new();
            for (tier, model) in [("top", next_top), ("implementer", next_impl), ("scout", next_scout)] {
                if !model.is_empty() {
                    models.insert(tier.to_string(), Value::String(model));
                }
            }
            config_changes.push(("cursor", serde_json::json!({ "models": models })));
            changed = true;
        }
    }
    if let Some(engines) = &change.engines {
        let lanes_input = engines.lanes.clone().unwrap_or_else(|| was.engines.lanes.clone());
        let Some(lanes) = config::lanes_of(&lanes_input) else {
            return Err("A lane runs on Claude Code or Cursor, or on the default.".to_string());
        };
        let next_default = engines.default.clone().unwrap_or_else(|| was.engines.default.clone());
        let rc = engines.roles.clone().unwrap_or_default();
        let next_roles = RolesView {
            planner: rc.planner.unwrap_or_else(|| was.engines.roles.planner.clone()),
            implementer: rc.implementer.unwrap_or_else(|| was.engines.roles.implementer.clone()),
            review: rc.review.unwrap_or_else(|| was.engines.roles.review.clone()),
            drafter: rc.drafter.unwrap_or_else(|| was.engines.roles.drafter.clone()),
            chat: rc.chat.unwrap_or_else(|| was.engines.roles.chat.clone()),
            writer: rc.writer.unwrap_or_else(|| was.engines.roles.writer.clone()),
        };
        let roles_ok = [
            &next_roles.planner,
            &next_roles.implementer,
            &next_roles.review,
            &next_roles.drafter,
            &next_roles.chat,
            &next_roles.writer,
        ]
        .iter()
        .all(|r| valid_engine_pick(r, true));
        if !valid_engine_pick(&next_default, false) || !roles_ok {
            return Err("An engine is Claude Code, Cursor or the local model.".to_string());
        }
        let lane_strs: Vec<String> = lanes.iter().map(ToString::to_string).collect();
        let was_lane_strs: Vec<String> = config::lanes_of(&was.engines.lanes)
            .unwrap_or_default()
            .iter()
            .map(ToString::to_string)
            .collect();
        if next_default != was.engines.default || next_roles != was.engines.roles || lane_strs != was_lane_strs {
            if let Some(why) = locked(SettingKey::Engines) {
                return Err(why);
            }
            let mut obj = Map::new();
            if next_default != "claude" {
                obj.insert("default".to_string(), Value::String(next_default));
            }
            let mut roles_obj = Map::new();
            for (name, value) in [
                ("planner", &next_roles.planner),
                ("implementer", &next_roles.implementer),
                ("review", &next_roles.review),
                ("drafter", &next_roles.drafter),
                ("chat", &next_roles.chat),
                ("writer", &next_roles.writer),
            ] {
                if value != "default" {
                    roles_obj.insert(name.to_string(), Value::String(value.clone()));
                }
            }
            if !roles_obj.is_empty() {
                obj.insert("roles".to_string(), Value::Object(roles_obj));
            }
            if !lane_strs.is_empty() {
                obj.insert(
                    "lanes".to_string(),
                    Value::Array(lane_strs.into_iter().map(Value::String).collect()),
                );
            }
            config_changes.push(("engineChoice", Value::Object(obj)));
            changed = true;
        }
    }
    if let Some(next_tags) = &change.tags {
        let next = tags::clean_tags(Value::Array(next_tags.iter().cloned().map(Value::String).collect()));
        if next != was.tags {
            if let Some(why) = locked(SettingKey::Tags) {
                return Err(why);
            }
            let removed = found.iter().filter(|t| !next.contains(t)).cloned().collect();
            tags_out = Some(tags::tag_overrides(found, Some(&TagOverrides { added: next, removed })));
            changed = true;
        }
    }
    if let Some(net) = &change.network {
        let enabled = net.enabled.unwrap_or(was.network.enabled);
        if enabled != was.network.enabled || net.password.is_some() {
            if let Some(why) = locked(SettingKey::Network) {
                return Err(why);
            }
            if let Some(p) = &net.password
                && p.encode_utf16().count() < network::MIN_PASSWORD_LEN
            {
                return Err(format!(
                    "The password is at least {} characters.",
                    network::MIN_PASSWORD_LEN
                ));
            }
            let password_hash = net
                .password
                .as_deref()
                .map(password::hash_password)
                .or_else(|| network_stored.and_then(|n| n.password_hash.clone()));
            if enabled && password_hash.is_none() {
                return Err("Set a password to turn network access on.".to_string());
            }
            restart = restart || enabled != was.network.enabled;
            network_out = Some(NetworkAccess { enabled, password_hash });
            changed = true;
        }
    }
    Ok(Applied {
        config_changes,
        tags: tags_out,
        network: network_out,
        restart,
        changed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn cfg() -> ConfigFile {
        ConfigFile::default()
    }

    #[test]
    fn current_settings_has_the_documented_defaults() {
        let (view, set_by) = current_settings(&cfg(), None, None, &[], no_env);
        assert_eq!(view.port, config::DEFAULT_PORT);
        assert_eq!(view.lanes, 2);
        assert!(!view.local.enabled);
        assert_eq!(view.poll_interval_sec, 10);
        assert_eq!(view.tags, Vec::<String>::new());
        assert_eq!(view.cursor_models.top, "");
        assert_eq!(view.engines.default, "claude");
        assert_eq!(view.engines.lanes, vec!["default", "default"]);
        assert!(!view.network.enabled);
        assert!(set_by.is_empty());
    }

    #[test]
    fn an_env_variable_locks_its_setting() {
        let env = |k: &str| (k == "RETASKABLE_PORT").then(|| "5000".to_string());
        let (view, set_by) = current_settings(&cfg(), None, None, &[], env);
        assert_eq!(view.port, 5000);
        assert_eq!(set_by.get("port"), Some(&"RETASKABLE_PORT".to_string()));

        let change = SettingsChange {
            port: Some(5001),
            ..Default::default()
        };
        let err = apply_settings(&change, &cfg(), None, None, &[], env).unwrap_err();
        assert_eq!(err, "The port is set by RETASKABLE_PORT: change it there.");
    }

    #[test]
    fn the_port_is_checked_and_applied() {
        let change = SettingsChange {
            port: Some(1023),
            ..Default::default()
        };
        assert_eq!(
            apply_settings(&change, &cfg(), None, None, &[], no_env).unwrap_err(),
            "The port is a whole number from 1024 to 65535."
        );
        let change = SettingsChange {
            port: Some(8080),
            ..Default::default()
        };
        let applied = apply_settings(&change, &cfg(), None, None, &[], no_env).unwrap();
        assert_eq!(applied.config_changes, vec![("port", Value::from(8080))]);
        assert!(applied.restart);
        assert!(applied.changed);
    }

    #[test]
    fn an_unchanged_value_changes_nothing() {
        let change = SettingsChange {
            port: Some(config::DEFAULT_PORT),
            lanes: Some(2),
            ..Default::default()
        };
        let applied = apply_settings(&change, &cfg(), None, None, &[], no_env).unwrap();
        assert!(applied.config_changes.is_empty());
        assert!(!applied.changed);
        assert!(!applied.restart);
    }

    #[test]
    fn the_local_model_keeps_the_files_own_address_when_only_the_model_changes() {
        let mut file = cfg();
        file.machine.local = config::LocalModelConfig {
            enabled: false,
            base_url: "http://localhost:11436".to_string(),
            model: String::new(),
        };
        // RETASKABLE_LOCAL_URL overrides the address the agent runs on, but a save of the model
        // alone must write the file's own address back, never the environment's.
        let env = |k: &str| (k == "RETASKABLE_LOCAL_URL").then(|| "http://10.0.0.5:9999".to_string());
        let change = SettingsChange {
            local: Some(LocalChange {
                enabled: Some(true),
                model: Some("llama3".to_string()),
                base_url: None,
            }),
            ..Default::default()
        };
        let applied = apply_settings(&change, &file, None, None, &[], env).unwrap();
        assert_eq!(
            applied.config_changes,
            vec![(
                "local",
                serde_json::json!({ "enabled": true, "model": "llama3", "baseUrl": "http://localhost:11436" })
            )]
        );
    }

    #[test]
    fn an_invalid_local_model_name_is_refused() {
        let change = SettingsChange {
            local: Some(LocalChange {
                enabled: Some(true),
                model: Some("bad name!".to_string()),
                base_url: None,
            }),
            ..Default::default()
        };
        assert_eq!(
            apply_settings(&change, &cfg(), None, None, &[], no_env).unwrap_err(),
            "The local model's name is up to 100 letters, digits and . _ : / -."
        );
    }

    #[test]
    fn cursor_models_save_only_the_non_empty_tiers() {
        let change = SettingsChange {
            cursor_models: Some(CursorModelsChange {
                top: Some("gpt-5".to_string()),
                implementer: None,
                scout: Some(String::new()),
            }),
            ..Default::default()
        };
        let applied = apply_settings(&change, &cfg(), None, None, &[], no_env).unwrap();
        assert_eq!(
            applied.config_changes,
            vec![("cursor", serde_json::json!({ "models": { "top": "gpt-5" } }))]
        );
    }

    #[test]
    fn engines_save_only_what_differs_from_claude_for_everything() {
        let change = SettingsChange {
            engines: Some(EnginesChange {
                default: Some("cursor".to_string()),
                roles: None,
                lanes: Some(vec!["claude".to_string(), "cursor".to_string()]),
            }),
            ..Default::default()
        };
        let applied = apply_settings(&change, &cfg(), None, None, &[], no_env).unwrap();
        assert_eq!(
            applied.config_changes,
            vec![(
                "engineChoice",
                serde_json::json!({ "default": "cursor", "lanes": ["claude", "cursor"] })
            )]
        );
    }

    #[test]
    fn a_lane_that_is_not_claude_cursor_or_default_is_refused() {
        let change = SettingsChange {
            engines: Some(EnginesChange {
                default: None,
                roles: None,
                lanes: Some(vec!["gpt5".to_string()]),
            }),
            ..Default::default()
        };
        assert_eq!(
            apply_settings(&change, &cfg(), None, None, &[], no_env).unwrap_err(),
            "A lane runs on Claude Code or Cursor, or on the default."
        );
    }

    #[test]
    fn tags_are_saved_as_the_minimal_change_from_found() {
        let found = vec!["windows".to_string(), "gpu".to_string()];
        let change = SettingsChange {
            tags: Some(vec!["windows".to_string(), "games".to_string()]),
            ..Default::default()
        };
        let applied = apply_settings(&change, &cfg(), None, None, &found, no_env).unwrap();
        assert_eq!(
            applied.tags,
            Some(TagOverrides {
                added: vec!["games".to_string()],
                removed: vec!["gpu".to_string()],
            })
        );
    }

    #[test]
    fn turning_network_access_on_needs_a_password_and_waits_for_a_restart() {
        let change = SettingsChange {
            network: Some(NetworkChange {
                enabled: Some(true),
                password: None,
            }),
            ..Default::default()
        };
        assert_eq!(
            apply_settings(&change, &cfg(), None, None, &[], no_env).unwrap_err(),
            "Set a password to turn network access on."
        );
        let change = SettingsChange {
            network: Some(NetworkChange {
                enabled: Some(true),
                password: Some("long enough password".to_string()),
            }),
            ..Default::default()
        };
        let applied = apply_settings(&change, &cfg(), None, None, &[], no_env).unwrap();
        assert!(applied.restart);
        let network = applied.network.unwrap();
        assert!(network.enabled);
        assert!(network.password_hash.is_some());
    }

    #[test]
    fn a_short_network_password_is_refused() {
        let change = SettingsChange {
            network: Some(NetworkChange {
                enabled: Some(true),
                password: Some("short".to_string()),
            }),
            ..Default::default()
        };
        assert_eq!(
            apply_settings(&change, &cfg(), None, None, &[], no_env).unwrap_err(),
            "The password is at least 8 characters."
        );
    }

    #[test]
    fn turning_network_access_off_keeps_the_hash() {
        let stored = NetworkAccess {
            enabled: true,
            password_hash: Some("scrypt$16384$8$1$aa$bb".to_string()),
        };
        let change = SettingsChange {
            network: Some(NetworkChange {
                enabled: Some(false),
                password: None,
            }),
            ..Default::default()
        };
        let applied = apply_settings(&change, &cfg(), None, Some(&stored), &[], no_env).unwrap();
        assert!(applied.restart);
        assert_eq!(
            applied.network,
            Some(NetworkAccess {
                enabled: false,
                password_hash: Some("scrypt$16384$8$1$aa$bb".to_string()),
            })
        );
    }
}
