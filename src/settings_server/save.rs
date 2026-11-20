//! The settings page's saves: each setting's validation and write, and the check of a whole
//! body before any of it is written (agent-settings.md sections 3.1 and 3.2).
use crate::text::is_blank;
use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use axum::response::Response;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{bad_request, internal_error};
use crate::config;
use crate::env::Env;
use crate::network::{self, NetworkAccess};
use crate::password;
use crate::store::MachineStore;
use crate::tags;

/// What a save needs: the agent's directory and environment. The settings page and `retaskable
/// link`'s setup questions (`setup_questions.rs`) both save through it.
pub(crate) struct Saver<'a> {
    pub(crate) dir: &'a Path,
    pub(crate) env: &'a Env,
}

impl Saver<'_> {
    /// Saves a typed tag list as changes to the found tags (added, removed), the same as setup
    /// will (agent-settings.md section 3.1).
    pub(crate) fn save_tags(&self, typed: &[String]) -> Result<()> {
        let env = &self.env;
        let found = tags::found_tags(&tags::machine_facts(env));
        let cleaned = tags::clean_tags(Value::Array(typed.iter().cloned().map(Value::String).collect()));
        let added = cleaned.iter().filter(|t| !found.contains(t)).cloned().collect();
        let removed = found.iter().filter(|t| !cleaned.contains(t)).cloned().collect();
        let overrides = tags::tag_overrides(&found, Some(&tags::TagOverrides { added, removed }));
        let mut store = MachineStore::load(self.dir)?;
        store.set_tags(overrides)
    }

    /// `Err` when `var` sets this setting (section 3.2): a save must refuse the same way the
    /// page greys the field out.
    pub(crate) fn env_gate(&self, var: &str) -> Result<(), SaveError> {
        if self.env.get(var).is_some() {
            return Err(SaveError::Invalid(format!("Set by {var}: change it there.")));
        }
        Ok(())
    }

    /// release-allowed-switch.md section 3.1 (`machineSettings.ts`): `releases` is `true`, `false`, or `null` to go
    /// back to the default. A value the switch already has is no change; changing it while `RETASKABLE_RELEASES` sets
    /// it is refused. `Ok(None)` is no change, `Ok(Some(v))` the new file value (`None` inside: remove it).
    pub(crate) fn validate_releases(&self, change: &Value) -> Result<Option<Option<bool>>, SaveError> {
        let file = config::load(self.dir)?;
        let env_sets = config::releases_switch(&config::ConfigFile::default(), self.env).is_some();
        match change {
            // Back to the default: only a saved choice needs removing, and an environment-set one isn't the file's.
            Value::Null => Ok((file.machine.releases.is_some() && !env_sets).then_some(None)),
            Value::Bool(on) => {
                if config::releases_switch(&file, self.env) == Some(*on) {
                    return Ok(None);
                }
                if env_sets {
                    return Err(SaveError::Invalid(
                        "Allowed to release is set by RETASKABLE_RELEASES: change it there.".to_string(),
                    ));
                }
                Ok(Some(Some(*on)))
            }
            _ => Err(SaveError::Invalid("Allowed to release is on or off.".to_string())),
        }
    }

    pub(crate) fn save_releases(&self, change: &Value) -> Result<(), SaveError> {
        if let Some(value) = self.validate_releases(change)? {
            config::save_machine_settings(self.dir, |obj| match value {
                Some(on) => {
                    obj.insert("releases".to_string(), json!(on));
                }
                None => {
                    obj.remove("releases");
                }
            })?;
        }
        Ok(())
    }

    /// agent-settings.md section 3.1: "Port: a whole number from 1024 to 65535." No write, so a
    /// caller can validate every field of a multi-field save before writing any of them.
    pub(crate) fn validate_port(&self, port: u16) -> Result<(), SaveError> {
        self.env_gate("RETASKABLE_PORT")?;
        if !config::port_in_range(port) {
            return Err(SaveError::Invalid("Port must be 1024 to 65535.".to_string()));
        }
        Ok(())
    }

    pub(crate) fn save_port(&self, port: u16) -> Result<(), SaveError> {
        self.validate_port(port)?;
        config::save_machine_settings(self.dir, |obj| {
            obj.insert("port".to_string(), json!(port));
        })?;
        Ok(())
    }

    /// agent-settings.md section 3.1: "How often it checks: 2 to 3600 seconds."
    pub(crate) fn validate_poll_interval(&self, secs: u32) -> Result<(), SaveError> {
        self.env_gate("RETASKABLE_POLL_SEC")?;
        if !config::poll_interval_in_range(secs) {
            return Err(SaveError::Invalid(
                "How often it checks must be 2 to 3600 seconds.".to_string(),
            ));
        }
        Ok(())
    }

    pub(crate) fn save_poll_interval(&self, secs: u32) -> Result<(), SaveError> {
        self.validate_poll_interval(secs)?;
        config::save_machine_settings(self.dir, |obj| {
            obj.insert("pollIntervalSec".to_string(), json!(secs));
        })?;
        Ok(())
    }

    /// agent-settings.md section 3.1: "CI slots: a whole number from 0 to 8."
    pub(crate) fn validate_ci_slots(&self, n: u32) -> Result<(), SaveError> {
        self.env_gate("RETASKABLE_CI_SLOTS")?;
        if !config::ci_slots_in_range(n) {
            return Err(SaveError::Invalid("CI slots must be 0 to 8.".to_string()));
        }
        Ok(())
    }

    pub(crate) fn save_ci_slots(&self, n: u32) -> Result<(), SaveError> {
        self.validate_ci_slots(n)?;
        config::save_machine_settings(self.dir, |obj| {
            obj.insert("ciSlots".to_string(), json!(n));
        })?;
        Ok(())
    }

    /// agent-settings.md section 3.1: "Local model: on or off; its model's name ...; its
    /// address, http or https, with no user or password in it."
    pub(crate) fn validate_local(&self, update: &LocalUpdate) -> Result<(), SaveError> {
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

    pub(crate) fn save_local(&self, update: &LocalUpdate) -> Result<(), SaveError> {
        self.validate_local(update)?;
        config::save_machine_settings(self.dir, |obj| {
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

    /// agent-settings.md section 3.1: "Company: one of claude, codex, cursor, pi or local", the one program all work runs on.
    pub(crate) fn validate_engine(&self, name: &str) -> Result<config::Engine, SaveError> {
        self.env_gate("RETASKABLE_ENGINE")?;
        self.env_gate("RETASKABLE_ENGINES")?;
        config::parse_engine(name)
            .ok_or_else(|| SaveError::Invalid("Company must be claude, codex, cursor, pi or local.".to_string()))
    }

    /// Saves the one company and drops the older per-kind and per-lane `engineChoice`, which it replaces (the older
    /// keys are only ever read, to migrate them: `config::engine_choice`).
    pub(crate) fn save_engine(&self, name: &str) -> Result<(), SaveError> {
        let engine = self.validate_engine(name)?;
        config::save_machine_settings(self.dir, |obj| {
            obj.insert("engine".to_string(), json!(engine.to_string()));
            obj.remove("engineChoice");
        })?;
        Ok(())
    }

    /// engines/06-permissions-the-allowlist.md section 6.4: a project's typed `engines.allow` entries,
    /// split on commas and new lines, each checked; none is saved when one is refused. Returns the
    /// project's index in `projects[]` and the entries.
    pub(crate) fn validate_cursor_allow(&self, update: &CursorAllowUpdate) -> Result<(usize, Vec<String>), SaveError> {
        let entries = crate::engines::cursor::split_allow_entries(&update.allow);
        if let Some(reason) = crate::engines::cursor::engines_allow_error(&entries) {
            return Err(SaveError::Invalid(reason));
        }
        let cfg = config::load(self.dir)?;
        let index = cfg
            .projects
            .iter()
            .position(|p| p.id_or_derived() == update.project)
            .ok_or_else(|| SaveError::Invalid("No such project.".to_string()))?;
        Ok((index, entries))
    }

    /// Sets the project's `engines.allow`, keeping the rest of its `engines`.
    pub(crate) fn save_cursor_allow(&self, update: &CursorAllowUpdate) -> Result<(), SaveError> {
        let (index, entries) = self.validate_cursor_allow(update)?;
        let mut found = false;
        config::save_machine_settings(self.dir, |obj| {
            let Some(project) = obj
                .get_mut("projects")
                .and_then(Value::as_array_mut)
                .and_then(|a| a.get_mut(index))
                .and_then(Value::as_object_mut)
            else {
                return;
            };
            found = true;
            let engines = project.entry("engines").or_insert_with(|| json!({}));
            if !engines.is_object() {
                *engines = json!({});
            }
            engines["allow"] = json!(entries);
        })?;
        if !found {
            return Err(SaveError::Invalid(
                "That project isn't in a projects list this agent can save to.".to_string(),
            ));
        }
        Ok(())
    }

    /// agent-settings.md section 3.1: "Cursor's models: one per tier ...; empty is auto."
    pub(crate) fn validate_cursor_models(&self, update: &CursorModelsUpdate) -> Result<CleanedCursorModels, SaveError> {
        self.env_gate("RETASKABLE_CURSOR_MODELS")?;
        let clean = |name: &Option<String>| -> Result<Option<String>, SaveError> {
            match name {
                None => Ok(None),
                Some(s) if is_blank(s) => Ok(None),
                Some(s) if config::valid_cursor_model_name(s) => Ok(Some(s.clone())),
                Some(_) => Err(SaveError::Invalid(
                    "A model name must be letters, digits, '_', '.', ':', '/' or '-', not starting with '-'."
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

    /// Saves the three tiers' models (`models.smart/fast/cheap`; `top`, `implementer` and `scout` of the update, in that
    /// order) and drops the older `cursor.models`, which they replace. An empty name goes back to the company's default.
    pub(crate) fn save_cursor_models(&self, update: &CursorModelsUpdate) -> Result<(), SaveError> {
        let cleaned = self.validate_cursor_models(update)?;
        config::save_machine_settings(self.dir, |obj| {
            let models = match obj.entry("models".to_string()).or_insert_with(|| json!({})) {
                Value::Object(m) => m,
                other => {
                    *other = json!({});
                    other.as_object_mut().expect("just set to an object")
                }
            };
            for (key, name) in [
                ("smart", cleaned.top),
                ("fast", cleaned.implementer),
                ("cheap", cleaned.scout),
            ] {
                match name {
                    Some(n) => models.insert(key.to_string(), json!(n)),
                    None => models.remove(key),
                };
            }
            if let Some(Value::Object(cursor)) = obj.get_mut("cursor") {
                cursor.remove("models");
            }
        })?;
        Ok(())
    }

    /// agent-settings.md section 3.1: "Network access: on or off. On needs a password of at
    /// least 8 characters ... Turning it off keeps the hash, so turning it on again needs no new
    /// one." Reads `machine.json` when `update` needs its already-saved hash, but never writes.
    pub(crate) fn validate_network(&self, update: &NetworkUpdate) -> Result<(), SaveError> {
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
            _ => MachineStore::load(self.dir)?
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

    pub(crate) fn save_network(&self, update: &NetworkUpdate) -> Result<(), SaveError> {
        self.validate_network(update)?;
        let mut store = MachineStore::load(self.dir)?;
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
    pub(crate) fn apply(&self, update: &SettingsUpdate) -> Result<(), SaveError> {
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
        if let Some(n) = update.ci_slots {
            self.validate_ci_slots(n)?;
        }
        if let Some(local) = &update.local {
            self.validate_local(local)?;
        }
        if let Some(engine) = &update.engine {
            self.validate_engine(engine)?;
        }
        if let Some(models) = &update.models {
            self.validate_cursor_models(&models.as_update())?;
        }
        if let Some(network) = &update.network {
            self.validate_network(network)?;
        }
        if let Some(allow) = &update.cursor_allow {
            self.validate_cursor_allow(allow)?;
        }
        if let Some(releases) = &update.releases {
            self.validate_releases(releases)?;
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
        if let Some(n) = update.ci_slots {
            self.save_ci_slots(n)?;
        }
        if let Some(local) = &update.local {
            self.save_local(local)?;
        }
        if let Some(engine) = &update.engine {
            self.save_engine(engine)?;
        }
        if let Some(models) = &update.models {
            self.save_cursor_models(&models.as_update())?;
        }
        if let Some(network) = &update.network {
            self.save_network(network)?;
        }
        if let Some(allow) = &update.cursor_allow {
            self.save_cursor_allow(allow)?;
        }
        if let Some(releases) = &update.releases {
            self.save_releases(releases)?;
        }
        Ok(())
    }
}

/// A save's refusal (section 3.1's own validation, or section 3.2's env-var gate) as opposed to
/// an unexpected I/O failure: the first answers 400, the second 500 (`save_error_response`).
pub(crate) enum SaveError {
    Invalid(String),
    Io(anyhow::Error),
}

impl From<anyhow::Error> for SaveError {
    fn from(e: anyhow::Error) -> Self {
        SaveError::Io(e)
    }
}

impl From<SaveError> for anyhow::Error {
    fn from(e: SaveError) -> Self {
        match e {
            SaveError::Invalid(message) => anyhow::anyhow!(message),
            SaveError::Io(e) => e,
        }
    }
}

pub(crate) fn save_error_response(e: SaveError) -> Response {
    match e {
        SaveError::Invalid(message) => bad_request(&message),
        SaveError::Io(e) => internal_error(&e.to_string()),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LocalUpdate {
    pub(crate) enabled: bool,
    pub(crate) model: String,
    pub(crate) base_url: String,
}

/// `models` in a save: the model each tier runs on; empty is the company's default.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct TierModelsUpdate {
    smart: Option<String>,
    fast: Option<String>,
    cheap: Option<String>,
}

impl TierModelsUpdate {
    /// Smart is Cursor's `top`, Fast its `implementer`, Cheap its `scout` (`CursorModels`).
    fn as_update(&self) -> CursorModelsUpdate {
        CursorModelsUpdate {
            top: self.smart.clone(),
            implementer: self.fast.clone(),
            scout: self.cheap.clone(),
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct CursorModelsUpdate {
    pub(crate) top: Option<String>,
    pub(crate) implementer: Option<String>,
    pub(crate) scout: Option<String>,
}

/// A `CursorModelsUpdate`'s names, validated and emptied to `None` (section 3.1: "empty is auto").
pub(crate) struct CleanedCursorModels {
    top: Option<String>,
    implementer: Option<String>,
    scout: Option<String>,
}

/// `cursorAllow`: one project's (by id) typed `engines.allow` text.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CursorAllowUpdate {
    pub(crate) project: String,
    pub(crate) allow: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NetworkUpdate {
    pub(crate) enabled: bool,
    #[serde(default)]
    pub(crate) password: Option<String>,
}

/// A field that is there, `null` included.
fn present<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(d).map(Some)
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct SettingsUpdate {
    pub(super) tags: Option<Vec<String>>,
    pub(super) port: Option<u16>,
    pub(super) poll_interval_sec: Option<u32>,
    pub(super) ci_slots: Option<u32>,
    pub(super) local: Option<LocalUpdate>,
    pub(super) engine: Option<String>,
    pub(super) models: Option<TierModelsUpdate>,
    pub(super) network: Option<NetworkUpdate>,
    pub(super) cursor_allow: Option<CursorAllowUpdate>,
    /// Allowed to release: `true`, `false` or `null` (back to the default); kept as a value so `null` isn't "absent".
    #[serde(deserialize_with = "present")]
    pub(super) releases: Option<Value>,
    /// `{ NAME: value | null }`: checked and written by `secrets.rs`, not by the `Saver`.
    pub(super) secrets: Option<Value>,
    #[serde(flatten)]
    pub(super) unsupported: HashMap<String, Value>,
}

#[cfg(test)]
mod tests;
