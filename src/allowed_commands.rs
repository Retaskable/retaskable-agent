//! The command fields of a project's shared settings, approved on the site before this computer runs them
//! (agent/08-http-api.md, the approval gate). Ports `agent/src/allowedCommands.ts` without its CI tree and
//! `releases` parts. A store version that changes what runs waits (`CommandAllowing::waiting`) until `approve`;
//! until then the allowed values run. Plain code: nothing here asks an LLM anything.
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map as JsonMap, Value};

use crate::config::{CheckConfig, ProjectConfig};
use crate::fs_util::write_json_atomic;
use crate::paths::project_dir;
use crate::telemetry::events::now_ms;

pub type Map = JsonMap<String, Value>;

/// Why an `approve_commands` for another version than the one waiting is refused.
pub const NOT_WAITING: &str =
    "Those aren't the commands waiting on this computer: look at the newest change and approve that.";

/// What each command field is called to the person approving it.
pub const COMMAND_LABELS: &[(&str, &str)] = &[
    ("checks", "checks"),
    ("playCommand", "the play command"),
    ("promptNotes", "the notes for the agent's prompts"),
    ("ciCommand", "the CI command"),
    ("phone", "the phone build"),
    ("engines", "what Cursor may run"),
    ("deploy.staging.smoke", "the staging smoke test"),
    ("deploy.production.rollout", "automatic rollout"),
];

const COMMAND_FIELDS: &[&str] = &["checks", "playCommand", "promptNotes", "ciCommand", "phone", "engines"];
const SMOKE: &str = "deploy.staging.smoke";
const ROLLOUT: &str = "deploy.production.rollout";
/// Minutes an automatic rollout takes (`FIXED_MINUTES` in `shared/rolloutSetting.ts`).
const ROLLOUT_MINUTES: u32 = 10;
const TEXT_LIMIT: usize = 2000;

/// What a computer must allow before it runs a change: the command fields, the staging smoke, and automatic
/// rollout whole, without which computer runs it (`machine`).
pub fn commands_of(values: &Value) -> Map {
    let mut out = Map::new();
    for f in COMMAND_FIELDS {
        if let Some(v) = values.get(f) {
            out.insert((*f).to_string(), v.clone());
        }
    }
    let deploy = values.get("deploy");
    if let Some(smoke) = deploy.and_then(|d| d.get("staging")).and_then(|s| s.get("smoke")) {
        out.insert(SMOKE.to_string(), smoke.clone());
    }
    if let Some(rollout) = deploy.and_then(|d| d.get("production")).and_then(|p| p.get("rollout")) {
        let mut rollout = rollout.clone();
        if let Some(o) = rollout.as_object_mut() {
            o.remove("machine");
        }
        out.insert(ROLLOUT.to_string(), rollout);
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

pub fn allowed_commands_file(data_dir: &Path, project: &str) -> PathBuf {
    project_dir(data_dir, project).join("allowed-commands.json")
}

impl AllowedCommands {
    /// The file, or None when there's none or it can't be read.
    pub fn read(file: &Path) -> Option<Self> {
        serde_json::from_str(&std::fs::read_to_string(file).ok()?).ok()
    }

    pub fn save(&self, file: &Path) -> anyhow::Result<()> {
        write_json_atomic(file, &serde_json::to_value(self)?, false)
    }
}

/// `cfg` with the allowed command fields laid over it. With a store record and nothing allowed, every command
/// field is empty: nothing runs until it is approved. With no record, `cfg` is as it was.
pub fn with_allowed(cfg: &ProjectConfig, allowed: Option<&AllowedCommands>, store_has_record: bool) -> ProjectConfig {
    let mut out = cfg.clone();
    let values = match allowed {
        Some(a) => &a.values,
        None if store_has_record => {
            out.checks = Vec::new();
            out.play_command = String::new();
            out.phone = None;
            out.engines = None;
            if let Some(s) = out.deploy.staging.as_mut() {
                s.smoke = None;
            }
            return out;
        }
        None => return cfg.clone(),
    };
    for (field, value) in values {
        match field.as_str() {
            "checks" => out.checks = serde_json::from_value::<Vec<CheckConfig>>(value.clone()).unwrap_or_default(),
            "playCommand" => out.play_command = value.as_str().unwrap_or_default().to_string(),
            "phone" => out.phone = serde_json::from_value(value.clone()).ok(),
            "engines" => out.engines = (!value.is_null()).then(|| value.clone()),
            SMOKE => {
                if let Some(s) = out.deploy.staging.as_mut() {
                    s.smoke = value.as_str().map(str::to_string);
                }
            }
            _ => {}
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
pub struct CommandChange {
    pub field: String,
    pub before: Option<Value>,
    pub after: Option<Value>,
}

/// What allowing the store's command fields would change in what this computer runs. A field the store has is its
/// value; one the store no longer has, that this computer allowed before, goes back to the file's.
pub fn command_changes(store: &Map, allowed: &Map, running: &Map, file: &Map) -> Vec<CommandChange> {
    let mut fields: Vec<&String> = store.keys().chain(allowed.keys()).collect();
    fields.sort();
    fields.dedup();
    let norm = |v: Option<&Value>| v.filter(|v| !v.is_null()).cloned();
    fields
        .into_iter()
        .filter_map(|f| {
            let after = norm(if store.contains_key(f) {
                store.get(f)
            } else {
                file.get(f)
            });
            let before = norm(running.get(f));
            (before != after).then(|| CommandChange {
                field: f.clone(),
                before,
                after,
            })
        })
        .collect()
}

/// A command field's value in a few words: the commands themselves, which is what someone allows.
fn command_text(field: &str, value: Option<&Value>) -> String {
    let none = || "nothing".to_string();
    let Some(v) = value.filter(|v| !v.is_null() && v.as_str() != Some("")) else {
        return none();
    };
    let s = |x: &Value, k: &str| x.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    match field {
        "checks" => {
            let list: Vec<String> = v
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|x| format!("{}: `{}`", s(x, "name"), s(x, "command")))
                        .collect()
                })
                .unwrap_or_default();
            if list.is_empty() { none() } else { list.join(", ") }
        }
        "phone" => format!("`{}`", s(v, "buildCommand")),
        "engines" => match v.get("allow").and_then(Value::as_array).filter(|a| !a.is_empty()) {
            Some(a) => format!(
                "allowing {}",
                a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")
            ),
            None => none(),
        },
        ROLLOUT => {
            let cmd = |k: &str| match s(v, k).trim() {
                "" => "none".to_string(),
                c => format!("`{c}`"),
            };
            let place = if v.get("site") == Some(&Value::Bool(true)) {
                "through retaskable.com".to_string()
            } else {
                format!("traffic {}, health {}", cmd("traffic"), cmd("health"))
            };
            format!("rolls out over {ROLLOUT_MINUTES} minutes, {place}")
        }
        _ => format!("`{}`", v.as_str().map_or_else(|| v.to_string(), str::to_string)),
    }
}

/// What new commands change, for their owner to approve: "checks: `npm test` → `npm run e2e`".
pub fn commands_change_text(changes: &[CommandChange]) -> String {
    let label = |f: &str| {
        COMMAND_LABELS
            .iter()
            .find(|(k, _)| *k == f)
            .map_or(f, |(_, l)| *l)
            .to_string()
    };
    let text = changes
        .iter()
        .map(|c| {
            format!(
                "{}: {} → {}",
                label(&c.field),
                command_text(&c.field, c.before.as_ref()),
                command_text(&c.field, c.after.as_ref())
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    text.chars().take(TEXT_LIMIT).collect()
}

/// One change in a settings record's history: who, and which top-level fields.
#[derive(Debug, Clone, Default)]
pub struct SettingsChange {
    pub by: String,
    pub fields: Vec<String>,
}

/// The parts of the store's settings record the gate reads.
#[derive(Debug, Clone, Default)]
pub struct StoreRecord {
    pub commands_version: u64,
    pub values: Value,
    pub updated_by: Option<String>,
    pub history: Vec<SettingsChange>,
}

/// Who last changed a command field, from the record's history.
fn changed_by(record: &StoreRecord) -> Option<String> {
    let is_command = |f: &String| COMMAND_FIELDS.contains(&f.as_str()) || f == "deploy";
    record
        .history
        .iter()
        .rev()
        .find(|c| c.fields.iter().any(is_command))
        .map(|c| c.by.clone())
        .or_else(|| record.updated_by.clone())
}

/// New commands waiting for the owner, with their values.
#[derive(Debug, Clone, PartialEq)]
pub struct Pending {
    pub version: u64,
    pub text: String,
    pub by: Option<String>,
    pub values: Map,
}

/// What the snapshot shows the site: the new commands waiting, without their values.
#[derive(Debug, Clone, PartialEq)]
pub struct Waiting {
    pub version: u64,
    pub text: String,
    pub by: Option<String>,
}

/// What `consider` and `approve` changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Moved {
    /// What waits changed: the snapshot says so.
    pub waiting: bool,
    /// The allowed values changed: rebuild the config.
    pub allowed: bool,
}

#[derive(Debug)]
pub struct CommandAllowing {
    file: PathBuf,
    pub allowed: Option<AllowedCommands>,
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

    pub fn waiting(&self) -> Option<Waiting> {
        self.pending.as_ref().map(|p| Waiting {
            version: p.version,
            text: p.text.clone(),
            by: p.by.clone(),
        })
    }

    /// The store's record (`record`), what runs now (`running`) and the file's (`file`), each as `commands_of`: when
    /// the store's commands moved past what was approved, they wait for `approve`. A version that changes nothing
    /// is taken without asking. `running` must be `commands_of` of what `with_allowed` gives now (empty before
    /// anything is approved), not the file's values, or a store that matches the file is taken unapproved.
    pub fn consider(&mut self, record: Option<&StoreRecord>, running: &Map, file: &Map) -> anyhow::Result<Moved> {
        let settled = self.allowed.as_ref().map_or(0, |a| a.commands_version);
        let Some(record) = record.filter(|r| r.commands_version > settled) else {
            return Ok(Moved {
                waiting: self.set_pending(None),
                allowed: false,
            });
        };
        let store = commands_of(&record.values);
        let empty = Map::new();
        let allowed_values = self.allowed.as_ref().map_or(&empty, |a| &a.values);
        let changes = command_changes(&store, allowed_values, running, file);
        if changes.is_empty() {
            let waiting = self.set_pending(None);
            let allowed = self.save(record.commands_version, store)?;
            return Ok(Moved { waiting, allowed });
        }
        if self
            .pending
            .as_ref()
            .is_some_and(|p| p.version == record.commands_version)
        {
            return Ok(Moved::default());
        }
        let pending = Pending {
            version: record.commands_version,
            text: commands_change_text(&changes),
            by: changed_by(record),
            values: store,
        };
        Ok(Moved {
            waiting: self.set_pending(Some(pending)),
            allowed: false,
        })
    }

    /// `approve_commands` from the site: runs `version`'s commands from now on. Refused when it isn't the version
    /// waiting.
    pub fn approve(&mut self, version: u64) -> Result<Moved, String> {
        let pending = match &self.pending {
            Some(p) if p.version == version => p.clone(),
            _ => return Err(NOT_WAITING.to_string()),
        };
        let allowed = self.save(version, pending.values).map_err(|e| e.to_string())?;
        Ok(Moved {
            waiting: self.set_pending(None),
            allowed,
        })
    }

    /// The project is unlinked or leaves the store: nothing waits any more.
    pub fn close(&mut self) {
        self.pending = None;
    }

    fn set_pending(&mut self, next: Option<Pending>) -> bool {
        let changed = next != self.pending;
        self.pending = next;
        changed
    }

    /// Saves the file and takes the values; true when they moved.
    fn save(&mut self, commands_version: u64, values: Map) -> anyhow::Result<bool> {
        let next = AllowedCommands {
            commands_version,
            values,
            at: now_ms(),
        };
        next.save(&self.file)?;
        // The first save always moves them: `with_allowed` gives None (nothing runs) and Some(empty) differently.
        let moved = self.allowed.as_ref().is_none_or(|a| a.values != next.values);
        self.allowed = Some(next);
        Ok(moved)
    }
}

#[cfg(test)]
mod tests;
