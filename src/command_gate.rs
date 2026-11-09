//! The approval gate's wiring (item `approve-commands-gate`, agent/08-http-api.md): one [`CommandAllowing`] per
//! project, shared by the loop that reads the store's settings and by everything that runs a project command, which
//! gets its `ProjectConfig` through [`CommandGates::config`] (or [`CommandGates::view`] for the connector's
//! `ProjectView`). Behind the `approve-commands-gate` release flag; Off, config.json's commands run as before.
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::{Value, json};

use crate::allowed_commands::{
    AllowedCommands, CommandAllowing, Moved, SettingsChange, StoreRecord, allowed_commands_file, commands_of,
    with_allowed,
};
use crate::backlog::settings::StoreSettings;
use crate::config::ProjectConfig;
use crate::view::ProjectView;

/// The release flag that turns the gate on, per project.
pub const FLAG: &str = "approve-commands-gate";

#[derive(Debug)]
struct Gate {
    allowing: CommandAllowing,
    /// Whether the store has settings for the project. Until the first read it is assumed: fail closed.
    has_record: bool,
}

#[derive(Clone, Default, Debug)]
pub struct CommandGates(
    Arc<Mutex<HashMap<String, Gate>>>,
    /// Counts the changes in what waits, across projects; the connector resends the snapshot on one.
    Arc<tokio::sync::watch::Sender<u64>>,
);

/// The command fields of `cfg`, as the store's settings values spell them.
fn command_values(cfg: &ProjectConfig) -> Value {
    let mut out = json!({
        "checks": cfg.checks,
        "playCommand": cfg.play_command,
        "galleries": cfg.galleries,
        "phone": cfg.phone,
        "engines": cfg.engines,
    });
    if let Some(staging) = &cfg.deploy.staging {
        out["deploy"] = json!({ "staging": { "smoke": staging.smoke } });
    }
    out
}

fn record_of(s: &StoreSettings) -> StoreRecord {
    StoreRecord {
        commands_version: s.commands_version,
        values: s.values.clone(),
        updated_by: s.updated_by.clone(),
        history: (s.history.iter())
            .map(|c| SettingsChange {
                by: c.by.clone(),
                fields: c.fields.iter().map(|f| f.field.clone()).collect(),
            })
            .collect(),
    }
}

impl CommandGates {
    fn lock(&self) -> MutexGuard<'_, HashMap<String, Gate>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Loads `project`'s allowed commands from `allowed-commands.json` (once).
    pub fn open(&self, data_dir: &Path, project: &str) {
        self.lock().entry(project.to_string()).or_insert_with(|| Gate {
            allowing: CommandAllowing::new(allowed_commands_file(data_dir, project)),
            has_record: true,
        });
    }

    /// What `cfg` is with the allowed commands over it. A project the loop never opened has nothing allowed.
    pub fn config(&self, project: &str, cfg: &ProjectConfig) -> ProjectConfig {
        match self.lock().get(project) {
            Some(g) => with_allowed(cfg, g.allowing.allowed.as_ref(), g.has_record),
            None => with_allowed(cfg, None, true),
        }
    }

    /// A successful `settings_get` of `project` (`None`: no settings yet) against config.json's `file`.
    pub fn consider(
        &self,
        project: &str,
        settings: Option<&StoreSettings>,
        file: &ProjectConfig,
    ) -> anyhow::Result<Moved> {
        let mut gates = self.lock();
        let Some(g) = gates.get_mut(project) else {
            return Ok(Moved::default());
        };
        g.has_record = settings.is_some();
        let running_cfg = with_allowed(file, g.allowing.allowed.as_ref(), g.has_record);
        let mut running = commands_of(&command_values(&running_cfg));
        // Fields the config has no place for (notes, the CI command, rollout) run as last allowed.
        if let Some(a) = &g.allowing.allowed {
            for (k, v) in &a.values {
                running.entry(k.clone()).or_insert_with(|| v.clone());
            }
        }
        let record = settings.map(record_of);
        let moved = g
            .allowing
            .consider(record.as_ref(), &running, &commands_of(&command_values(file)))?;
        drop(gates);
        self.changed(moved);
        Ok(moved)
    }

    /// Subscribes to changes in what waits (one starts waiting, is replaced or is approved); the value only counts them.
    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<u64> {
        self.1.subscribe()
    }

    fn changed(&self, moved: Moved) {
        if moved.waiting {
            self.1.send_modify(|n| *n += 1);
        }
    }

    /// `project`'s waiting version as the snapshot's `commandsWaiting`, with the flag On (`on`) and one waiting.
    pub fn waiting_json(&self, project: &str, on: bool) -> Option<Value> {
        let w = self.lock().get(project)?.allowing.waiting().filter(|_| on)?;
        Some(json!({ "version": w.version, "text": w.text, "by": w.by }))
    }

    /// `approve_commands`: runs the waiting `version`'s commands from now on.
    pub fn approve(&self, project: &str, version: u64) -> Result<Moved, String> {
        let moved = match self.lock().get_mut(project) {
            Some(g) => g.allowing.approve(version),
            None => Err(crate::allowed_commands::NOT_WAITING.to_string()),
        }?;
        self.changed(moved);
        Ok(moved)
    }

    /// The allowed commands as saved, for a test or the snapshot.
    pub fn allowed(&self, project: &str) -> Option<AllowedCommands> {
        self.lock().get(project)?.allowing.allowed.clone()
    }

    /// `view` with its galleries and play command as allowed.
    pub fn view(&self, project: &ProjectView) -> ProjectView {
        let cfg = ProjectConfig {
            galleries: project.galleries.clone(),
            play_command: project.play_command.clone(),
            ..ProjectConfig::default()
        };
        let cfg = self.config(&project.id, &cfg);
        ProjectView {
            galleries: cfg.galleries,
            play_command: cfg.play_command,
            ..project.clone()
        }
    }
}

#[cfg(test)]
mod tests;
