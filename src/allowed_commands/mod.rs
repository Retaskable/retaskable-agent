//! The command fields of a shared project's settings, approved before this computer runs them (agent/08-http-api.md,
//! "The approval gate"; `agent/src/allowedCommands.ts` in Retaskable.com, without its CI tree and releases parts).
//! When the store's `commandsVersion` moves past what the owner approved, the new commands wait; `approve` takes the
//! waiting version. Plain code: nothing here asks an LLM anything. Nothing calls it yet.
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Map as JsonMap, Value};

use crate::config::{CheckConfig, GalleryConfig, ProjectConfig};
use crate::fs_util::write_json_atomic;
use crate::paths::project_dir;
use crate::text::truncate;

pub type Map = JsonMap<String, Value>;

const ROLLOUT: &str = "deploy.production.rollout";
const SMOKE: &str = "deploy.staging.smoke";
/// The rollout's length in minutes (`FIXED_MINUTES` in `shared/rolloutSetting.ts`).
const ROLLOUT_MINUTES: u32 = 10;
/// The settings fields that are commands (`settingFields("command")`).
const COMMAND_SETTINGS: [&str; 7] = [
    "checks",
    "playCommand",
    "promptNotes",
    "ciCommand",
    "galleries",
    "phone",
    "engines",
];

/// What each command field is called to the person approving it (`COMMAND_LABELS`).
pub const COMMAND_LABELS: [(&str, &str); 9] = [
    ("checks", "checks"),
    ("playCommand", "the play command"),
    ("promptNotes", "the notes for the agent's prompts"),
    ("ciCommand", "the CI command"),
    ("galleries", "galleries"),
    ("phone", "the phone build"),
    ("engines", "what Cursor may run"),
    (SMOKE, "the staging smoke test"),
    (ROLLOUT, "automatic rollout"),
];

/// Why an `approve` for another version than the one waiting is refused.
pub const NOT_WAITING: &str =
    "Those aren't the commands waiting on this computer: look at the newest change and approve that.";

fn label(field: &str) -> &str {
    COMMAND_LABELS
        .iter()
        .find(|(f, _)| *f == field)
        .map_or(field, |(_, l)| l)
}

/// The command fields of a store settings record's `values` (`commandsOf`): the rollout's `machine` is left out.
pub fn commands_of(values: &Value) -> Map {
    let mut out = Map::new();
    for f in COMMAND_SETTINGS {
        if let Some(v) = values.get(f) {
            out.insert(f.into(), v.clone());
        }
    }
    let deploy = values.get("deploy");
    if let Some(smoke) = deploy.and_then(|d| d.pointer("/staging/smoke")) {
        out.insert(SMOKE.into(), smoke.clone());
    }
    if let Some(rollout) = deploy.and_then(|d| d.pointer("/production/rollout")) {
        let mut rollout = rollout.clone();
        if let Some(o) = rollout.as_object_mut() {
            o.remove("machine");
        }
        out.insert(ROLLOUT.into(), rollout);
    }
    out
}

/// What this computer agreed to run: the store's command fields at `commands_version`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AllowedCommands {
    pub commands_version: u64,
    pub values: Map,
    pub at: i64,
}

impl AllowedCommands {
    pub fn file(data_dir: &Path, project: &str) -> PathBuf {
        project_dir(data_dir, project).join("allowed-commands.json")
    }

    /// The file, or None when there's none or it can't be read.
    pub fn read(file: &Path) -> Option<Self> {
        serde_json::from_str(&std::fs::read_to_string(file).ok()?).ok()
    }

    pub fn save(&self, file: &Path) -> anyhow::Result<()> {
        write_json_atomic(file, &serde_json::to_value(self)?, false)
    }
}

/// `cfg` with the allowed commands laid over it. A store record with nothing allowed yet allows nothing: every
/// command field is empty. Without a store record the config is as it is. (The config has no automatic rollout yet.)
pub fn with_allowed(cfg: &ProjectConfig, allowed: Option<&AllowedCommands>, store_has_record: bool) -> ProjectConfig {
    if !store_has_record {
        return cfg.clone();
    }
    let mut out = cfg.clone();
    let none = Map::new();
    let values = allowed.map_or(&none, |a| &a.values);
    let all_empty = allowed.is_none();
    let get = |f: &str| values.get(f).cloned();
    if all_empty || values.contains_key("checks") {
        out.checks = get("checks")
            .and_then(|v| serde_json::from_value::<Vec<CheckConfig>>(v).ok())
            .unwrap_or_default();
    }
    if all_empty || values.contains_key("playCommand") {
        out.play_command = get("playCommand")
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_default();
    }
    if all_empty || values.contains_key("galleries") {
        out.galleries = get("galleries")
            .and_then(|v| serde_json::from_value::<Vec<GalleryConfig>>(v).ok())
            .unwrap_or_default();
    }
    if all_empty || values.contains_key("phone") {
        out.phone = get("phone").and_then(|v| serde_json::from_value(v).ok());
    }
    if all_empty || values.contains_key("engines") {
        out.engines = get("engines").filter(|v| !v.is_null());
    }
    if (all_empty || values.contains_key(SMOKE))
        && let Some(staging) = out.deploy.staging.as_mut()
    {
        staging.smoke = get(SMOKE).and_then(|v| v.as_str().map(String::from));
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
pub struct CommandChange {
    pub field: String,
    pub before: Value,
    pub after: Value,
}

/// What allowing the store's command fields would change in what this computer runs. A field the store has is its
/// value; one the store no longer has, that was allowed before, goes back to the file's.
pub fn command_changes(store: &Map, allowed: &Map, running: &Map, file: &Map) -> Vec<CommandChange> {
    let mut fields: Vec<&String> = store.keys().collect();
    fields.extend(allowed.keys().filter(|k| !store.contains_key(*k)));
    let mut out = Vec::new();
    for field in fields {
        let after = if store.contains_key(field) {
            store.get(field)
        } else {
            file.get(field)
        };
        let before = running.get(field);
        if before.unwrap_or(&Value::Null) != after.unwrap_or(&Value::Null) {
            out.push(CommandChange {
                field: field.clone(),
                before: before.cloned().unwrap_or(Value::Null),
                after: after.cloned().unwrap_or(Value::Null),
            });
        }
    }
    out
}

fn named(value: &Value) -> String {
    let list: Vec<String> = value
        .as_array()
        .map(|a| {
            a.iter()
                .map(|x| format!("{}: `{}`", text_of(&x["name"]), text_of(&x["command"])))
                .collect()
        })
        .unwrap_or_default();
    if list.is_empty() {
        "nothing".into()
    } else {
        list.join(", ")
    }
}

fn text_of(v: &Value) -> String {
    match v {
        Value::Null => "undefined".into(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// A command field's value in a few words: the commands themselves, which is what someone allows.
fn command_text(field: &str, value: &Value) -> String {
    if value.is_null() || value.as_str() == Some("") {
        return "nothing".into();
    }
    match field {
        "checks" | "galleries" => named(value),
        "phone" => format!("`{}`", text_of(&value["buildCommand"])),
        "engines" => match value["allow"].as_array().filter(|a| !a.is_empty()) {
            Some(a) => format!("allowing {}", a.iter().map(text_of).collect::<Vec<_>>().join(", ")),
            None => "nothing".into(),
        },
        ROLLOUT => {
            let cmd = |k: &str| match value[k].as_str().map(str::trim) {
                Some(c) if !c.is_empty() => format!("`{c}`"),
                _ => "none".into(),
            };
            let place = if value["site"] == Value::Bool(true) {
                "through retaskable.com".to_string()
            } else {
                format!("traffic {}, health {}", cmd("traffic"), cmd("health"))
            };
            format!("rolls out over {ROLLOUT_MINUTES} minutes, {place}")
        }
        _ => format!("`{}`", text_of(value)),
    }
}

/// What new commands change, for their owner to approve: "checks: `npm test` → `npm run e2e`".
pub fn commands_change_text(changes: &[CommandChange]) -> String {
    let text = changes
        .iter()
        .map(|c| {
            format!(
                "{}: {} → {}",
                label(&c.field),
                command_text(&c.field, &c.before),
                command_text(&c.field, &c.after)
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    truncate(&text, 2000)
}

/// A store settings record, as far as the gate reads it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SettingsRecord {
    pub commands_version: u64,
    pub values: Value,
    pub updated_by: Option<String>,
    /// The latest changes, oldest first.
    pub history: Vec<SettingsChange>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SettingsChange {
    pub by: String,
    pub fields: Vec<String>,
}

/// Who last changed a command field, from the record's history.
fn changed_by(record: &SettingsRecord) -> Option<String> {
    let is_command = |f: &String| COMMAND_SETTINGS.contains(&f.as_str()) || f == "deploy" || f == "nfrs";
    let last = record.history.iter().rev().find(|c| c.fields.iter().any(is_command));
    last.map(|c| c.by.clone()).or_else(|| record.updated_by.clone())
}

/// What the snapshot shows the site: the new commands waiting, without their values.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandsWaiting {
    pub version: u64,
    pub text: String,
    pub by: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pending {
    pub version: u64,
    pub text: String,
    pub by: Option<String>,
    pub values: Map,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

pub struct CommandAllowing {
    file: PathBuf,
    pub allowed: Option<AllowedCommands>,
    /// The store's new commands, waiting for the owner to approve them on the site.
    pub pending: Option<Pending>,
}

impl CommandAllowing {
    pub fn new(file: PathBuf) -> Self {
        let allowed = AllowedCommands::read(&file);
        Self {
            file,
            allowed,
            pending: None,
        }
    }

    pub fn waiting(&self) -> Option<CommandsWaiting> {
        self.pending.as_ref().map(|p| CommandsWaiting {
            version: p.version,
            text: p.text.clone(),
            by: p.by.clone(),
        })
    }

    /// The settings it runs on (`record`), what it runs now (`running`) and the file's (`file`), each as
    /// `commands_of`: when the store's commands moved past what was approved, they wait for `approve`. A change that
    /// wouldn't change what runs is taken without asking. True when what is waiting changed.
    pub fn consider(&mut self, record: Option<&SettingsRecord>, running: &Map, file: &Map) -> bool {
        let settled = self.allowed.as_ref().map_or(0, |a| a.commands_version);
        let Some(record) = record.filter(|r| r.commands_version > settled) else {
            return self.set_pending(None);
        };
        let version = record.commands_version;
        let store = commands_of(&record.values);
        let none = Map::new();
        let changes = command_changes(
            &store,
            self.allowed.as_ref().map_or(&none, |a| &a.values),
            running,
            file,
        );
        if changes.is_empty() {
            let changed = self.set_pending(None);
            self.keep(AllowedCommands {
                commands_version: version,
                values: store,
                at: now_ms(),
            });
            return changed;
        }
        if self.pending.as_ref().is_some_and(|p| p.version == version) {
            return false;
        }
        self.set_pending(Some(Pending {
            version,
            text: commands_change_text(&changes),
            by: changed_by(record),
            values: store,
        }))
    }

    /// `approve_commands` from the site: runs `version`'s commands from now on. Refused when it isn't the version
    /// waiting. Ok(true) when the allowed values moved.
    pub fn approve(&mut self, version: u64) -> Result<bool, String> {
        let pending = match self.pending.take() {
            Some(p) if p.version == version => p,
            other => {
                self.pending = other;
                return Err(NOT_WAITING.into());
            }
        };
        let next = AllowedCommands {
            commands_version: version,
            values: pending.values,
            at: now_ms(),
        };
        if let Err(e) = next.save(&self.file) {
            self.pending = Some(Pending {
                values: next.values,
                ..pending
            });
            return Err(format!("couldn't save the approved commands: {e}"));
        }
        let moved = self.allowed.as_ref().map(|a| &a.values) != Some(&next.values);
        self.allowed = Some(next);
        Ok(moved)
    }

    /// The project is unlinked or leaves the store: nothing waits any more.
    pub fn close(&mut self) {
        self.pending = None;
    }

    fn set_pending(&mut self, next: Option<Pending>) -> bool {
        if next == self.pending {
            return false;
        }
        self.pending = next;
        true
    }

    /// Takes a version that changes nothing. A file that can't be written leaves it in memory only: the next
    /// start asks again.
    fn keep(&mut self, next: AllowedCommands) {
        let _ = next.save(&self.file);
        self.allowed = Some(next);
    }
}

#[cfg(test)]
mod tests;
