//! The settings page's saves: each setting's validation and write, and the check of a whole
//! body before any of it is written (agent-settings.md sections 3.1 and 3.2).
use std::collections::HashMap;

use anyhow::Result;
use axum::response::Response;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::{SettingsServer, bad_request, internal_error};
use crate::config;
use crate::network::{self, NetworkAccess};
use crate::password;
use crate::store::MachineStore;
use crate::tags;

impl SettingsServer {
    /// Saves a typed tag list as changes to the found tags (added, removed), the same as setup
    /// will (agent-settings.md section 3.1).
    pub(super) fn save_tags(&self, typed: &[String]) -> Result<()> {
        let env = &self.env;
        let found = tags::found_tags(&tags::machine_facts(env));
        let cleaned = tags::clean_tags(Value::Array(typed.iter().cloned().map(Value::String).collect()));
        let added = cleaned.iter().filter(|t| !found.contains(t)).cloned().collect();
        let removed = found.iter().filter(|t| !cleaned.contains(t)).cloned().collect();
        let overrides = tags::tag_overrides(&found, Some(&tags::TagOverrides { added, removed }));
        let mut store = MachineStore::load(&self.dir)?;
        store.set_tags(overrides)
    }

    /// `Err` when `var` sets this setting (section 3.2): a save must refuse the same way the
    /// page greys the field out.
    pub(super) fn env_gate(&self, var: &str) -> Result<(), SaveError> {
        if self.env.get(var).is_some() {
            return Err(SaveError::Invalid(format!("Set by {var}: change it there.")));
        }
        Ok(())
    }

    /// agent-settings.md section 3.1: "Port: a whole number from 1024 to 65535." No write, so a
    /// caller can validate every field of a multi-field save before writing any of them.
    pub(super) fn validate_port(&self, port: u16) -> Result<(), SaveError> {
        self.env_gate("RETASKABLE_PORT")?;
        if !config::port_in_range(port) {
            return Err(SaveError::Invalid("Port must be 1024 to 65535.".to_string()));
        }
        Ok(())
    }

    pub(super) fn save_port(&self, port: u16) -> Result<(), SaveError> {
        self.validate_port(port)?;
        config::save_machine_settings(&self.dir, |obj| {
            obj.insert("port".to_string(), json!(port));
        })?;
        Ok(())
    }

    /// agent-settings.md section 3.1: "How often it checks: 2 to 3600 seconds."
    pub(super) fn validate_poll_interval(&self, secs: u32) -> Result<(), SaveError> {
        self.env_gate("RETASKABLE_POLL_SEC")?;
        if !config::poll_interval_in_range(secs) {
            return Err(SaveError::Invalid(
                "How often it checks must be 2 to 3600 seconds.".to_string(),
            ));
        }
        Ok(())
    }

    pub(super) fn save_poll_interval(&self, secs: u32) -> Result<(), SaveError> {
        self.validate_poll_interval(secs)?;
        config::save_machine_settings(&self.dir, |obj| {
            obj.insert("pollIntervalSec".to_string(), json!(secs));
        })?;
        Ok(())
    }

    /// agent-settings.md section 3.1: "Local model: on or off; its model's name ...; its
    /// address, http or https, with no user or password in it."
    pub(super) fn validate_local(&self, update: &LocalUpdate) -> Result<(), SaveError> {
        self.env_gate("RETASKABLE_LOCAL_MODEL")?;
        self.env_gate("RETASKABLE_LOCAL_URL")?;
        if update.enabled && !config::valid_model_name(&update.model) {
            return Err(SaveError::Invalid(
                "Local model's name must be 1 to 100 letters, digits, '.', '_', ':', '/' or '-'.".to_string(),
            ));
        }
        if !config::valid_local_url(&update.base_url) {
            return Err(SaveError::Invalid(
                "Local model's address must be http or https, with no user or password in it.".to_string(),
            ));
        }
        Ok(())
    }

    pub(super) fn save_local(&self, update: &LocalUpdate) -> Result<(), SaveError> {
        self.validate_local(update)?;
        config::save_machine_settings(&self.dir, |obj| {
            obj.insert(
                "local".to_string(),
                json!({
                    "enabled": update.enabled,
                    "model": update.model,
                    "baseUrl": update.base_url,
                }),
            );
        })?;
        Ok(())
    }

    /// agent-settings.md section 3.1: "Engines: a default (claude, cursor, pi, codex or local) and, per
    /// kind of work ..., the default or one of the four", plus the per-lane override below it.
    /// `roles` and `lanes` entries are always present in the request (the page's one "Engines"
    /// save sends the whole group), so this both parses and validates them.
    pub(super) fn validate_engine_choice(
        &self,
        update: &EngineChoiceUpdate,
    ) -> Result<config::EngineChoiceConfig, SaveError> {
        self.env_gate("RETASKABLE_ENGINES")?;
        let default = config::parse_engine(&update.default)
            .ok_or_else(|| SaveError::Invalid("Engine must be claude, cursor, pi, codex or local.".to_string()))?;
        let roles = config::RoleEngines {
            planner: parse_role_engine(&update.roles.planner)?,
            implementer: parse_role_engine(&update.roles.implementer)?,
            review: parse_role_engine(&update.roles.review)?,
            drafter: parse_role_engine(&update.roles.drafter)?,
            chat: parse_role_engine(&update.roles.chat)?,
            writer: parse_role_engine(&update.roles.writer)?,
        };
        let lanes = config::lanes_of(&update.lanes)
            .ok_or_else(|| SaveError::Invalid("Lanes must be claude, cursor, codex or default.".to_string()))?;
        Ok(config::EngineChoiceConfig {
            default: Some(default),
            roles,
            lanes,
        })
    }

    pub(super) fn save_engine_choice(&self, update: &EngineChoiceUpdate) -> Result<(), SaveError> {
        let engine_choice = self.validate_engine_choice(update)?;
        config::save_machine_settings(&self.dir, |obj| {
            obj.insert("engineChoice".to_string(), engine_choice_json(&engine_choice));
        })?;
        Ok(())
    }

    /// agent-settings.md section 3.1: "Cursor's models: one per tier ...; empty is auto."
    pub(super) fn validate_cursor_models(&self, update: &CursorModelsUpdate) -> Result<CleanedCursorModels, SaveError> {
        self.env_gate("RETASKABLE_CURSOR_MODELS")?;
        let clean = |name: &Option<String>| -> Result<Option<String>, SaveError> {
            match name {
                None => Ok(None),
                Some(s) if s.trim().is_empty() => Ok(None),
                Some(s) if config::valid_cursor_model_name(s) => Ok(Some(s.clone())),
                Some(_) => Err(SaveError::Invalid(
                    "Cursor's model name must be letters, digits, '_', '.', ':', '/' or '-', not starting with '-'."
                        .to_string(),
                )),
            }
        };
        Ok(CleanedCursorModels {
            top: clean(&update.top)?,
            implementer: clean(&update.implementer)?,
            scout: clean(&update.scout)?,
        })
    }

    pub(super) fn save_cursor_models(&self, update: &CursorModelsUpdate) -> Result<(), SaveError> {
        let cleaned = self.validate_cursor_models(update)?;
        config::save_machine_settings(&self.dir, |obj| {
            let mut models = Map::new();
            if let Some(t) = cleaned.top {
                models.insert("top".to_string(), json!(t));
            }
            if let Some(i) = cleaned.implementer {
                models.insert("implementer".to_string(), json!(i));
            }
            if let Some(s) = cleaned.scout {
                models.insert("scout".to_string(), json!(s));
            }
            match obj.entry("cursor".to_string()).or_insert_with(|| json!({})) {
                Value::Object(cursor) => {
                    cursor.insert("models".to_string(), Value::Object(models));
                }
                other => *other = json!({ "models": models }),
            }
        })?;
        Ok(())
    }

    /// agent-settings.md section 3.1: "Network access: on or off. On needs a password of at
    /// least 8 characters ... Turning it off keeps the hash, so turning it on again needs no new
    /// one." Reads `machine.json` when `update` needs its already-saved hash, but never writes.
    pub(super) fn validate_network(&self, update: &NetworkUpdate) -> Result<(), SaveError> {
        self.env_gate("RETASKABLE_NETWORK_PASSWORD")?;
        let has_password = match &update.password {
            Some(p) if !p.is_empty() => {
                if p.encode_utf16().count() < network::MIN_PASSWORD_LEN {
                    return Err(SaveError::Invalid(format!(
                        "Password must be at least {} characters.",
                        network::MIN_PASSWORD_LEN
                    )));
                }
                true
            }
            _ => MachineStore::load(&self.dir)?
                .network()
                .is_some_and(|n| n.password_hash.is_some()),
        };
        if update.enabled && !has_password {
            return Err(SaveError::Invalid(
                "Network access needs a password of at least 8 characters.".to_string(),
            ));
        }
        Ok(())
    }

    pub(super) fn save_network(&self, update: &NetworkUpdate) -> Result<(), SaveError> {
        self.validate_network(update)?;
        let mut store = MachineStore::load(&self.dir)?;
        let password_hash = match &update.password {
            Some(p) if !p.is_empty() => Some(password::hash_password(p)),
            _ => store.network().and_then(|n| n.password_hash),
        };
        store.set_network(NetworkAccess {
            enabled: update.enabled,
            password_hash,
        })?;
        Ok(())
    }

    /// Validates every field this body touches before writing any of them, so a body with a good
    /// field and a bad one (e.g. tags plus an out-of-range port) never leaves a half-done save.
    /// Then writes each, in order.
    pub(super) fn apply(&self, update: &SettingsUpdate) -> Result<(), SaveError> {
        self.validate_update(update)?;
        self.save_update(update)
    }

    fn validate_update(&self, update: &SettingsUpdate) -> Result<(), SaveError> {
        if let Some(port) = update.port {
            self.validate_port(port)?;
        }
        if let Some(secs) = update.poll_interval_sec {
            self.validate_poll_interval(secs)?;
        }
        if let Some(local) = &update.local {
            self.validate_local(local)?;
        }
        if let Some(engine_choice) = &update.engine_choice {
            self.validate_engine_choice(engine_choice)?;
        }
        if let Some(cursor_models) = &update.cursor_models {
            self.validate_cursor_models(cursor_models)?;
        }
        if let Some(network) = &update.network {
            self.validate_network(network)?;
        }
        Ok(())
    }

    fn save_update(&self, update: &SettingsUpdate) -> Result<(), SaveError> {
        if let Some(typed) = &update.tags {
            self.save_tags(typed)?;
        }
        if let Some(port) = update.port {
            self.save_port(port)?;
        }
        if let Some(secs) = update.poll_interval_sec {
            self.save_poll_interval(secs)?;
        }
        if let Some(local) = &update.local {
            self.save_local(local)?;
        }
        if let Some(engine_choice) = &update.engine_choice {
            self.save_engine_choice(engine_choice)?;
        }
        if let Some(cursor_models) = &update.cursor_models {
            self.save_cursor_models(cursor_models)?;
        }
        if let Some(network) = &update.network {
            self.save_network(network)?;
        }
        Ok(())
    }
}

/// A save's refusal (section 3.1's own validation, or section 3.2's env-var gate) as opposed to
/// an unexpected I/O failure: the first answers 400, the second 500 (`save_error_response`).
pub(super) enum SaveError {
    Invalid(String),
    Io(anyhow::Error),
}

impl From<anyhow::Error> for SaveError {
    fn from(e: anyhow::Error) -> Self {
        SaveError::Io(e)
    }
}

/// `engineChoice` as both `GET /api/settings` returns it and `config.json` stores it: the same
/// shape either way, so a save can write back exactly what a read would show.
pub(super) fn engine_choice_json(cfg: &config::EngineChoiceConfig) -> Value {
    json!({
        "default": cfg.default.map(|e| e.to_string()),
        "roles": {
            "planner": cfg.roles.planner.map(|e| e.to_string()),
            "implementer": cfg.roles.implementer.map(|e| e.to_string()),
            "review": cfg.roles.review.map(|e| e.to_string()),
            "drafter": cfg.roles.drafter.map(|e| e.to_string()),
            "chat": cfg.roles.chat.map(|e| e.to_string()),
            "writer": cfg.roles.writer.map(|e| e.to_string()),
        },
        "lanes": cfg.lanes.iter().map(ToString::to_string).collect::<Vec<_>>(),
    })
}

/// A role's own override (agent-settings.md section 3.1): empty or `default` follows the default
/// engine (`None`), anything else must be one of the four engines.
pub(super) fn parse_role_engine(value: &str) -> Result<Option<config::Engine>, SaveError> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("default") {
        return Ok(None);
    }
    config::parse_engine(trimmed)
        .map(Some)
        .ok_or_else(|| SaveError::Invalid("Engine must be claude, cursor, pi, codex, local or default.".to_string()))
}

pub(super) fn save_error_response(e: SaveError) -> Response {
    match e {
        SaveError::Invalid(message) => bad_request(&message),
        SaveError::Io(e) => internal_error(&e.to_string()),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct LocalUpdate {
    enabled: bool,
    model: String,
    base_url: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct EngineChoiceUpdate {
    default: String,
    #[serde(default)]
    roles: RoleEngineUpdate,
    #[serde(default)]
    lanes: Vec<String>,
}

/// `engineChoice.roles`' raw request shape: each field is the role's own engine name, `default`
/// or empty to follow the default engine (`parse_role_engine`).
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct RoleEngineUpdate {
    planner: String,
    implementer: String,
    review: String,
    drafter: String,
    chat: String,
    writer: String,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct CursorModelsUpdate {
    top: Option<String>,
    implementer: Option<String>,
    scout: Option<String>,
}

/// A `CursorModelsUpdate`'s names, validated and emptied to `None` (section 3.1: "empty is auto").
pub(super) struct CleanedCursorModels {
    top: Option<String>,
    implementer: Option<String>,
    scout: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct NetworkUpdate {
    enabled: bool,
    #[serde(default)]
    password: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct SettingsUpdate {
    pub(super) tags: Option<Vec<String>>,
    pub(super) port: Option<u16>,
    pub(super) poll_interval_sec: Option<u32>,
    pub(super) local: Option<LocalUpdate>,
    pub(super) engine_choice: Option<EngineChoiceUpdate>,
    pub(super) cursor_models: Option<CursorModelsUpdate>,
    pub(super) network: Option<NetworkUpdate>,
    #[serde(flatten)]
    pub(super) unsupported: HashMap<String, Value>,
}

#[cfg(test)]
mod tests;
