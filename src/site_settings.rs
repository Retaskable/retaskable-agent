//! The owner's agent settings, changed from the site (agent-settings.md section 2.1; `siteSettings.ts` in the
//! TypeScript agent): the daily budget, sessions at once and the model thresholds. Each field is checked as Setup
//! checks it, and what isn't named stays in `config.json`.
use std::path::Path;

use anyhow::{Result, anyhow};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::config;

/// `agent_settings { dailyUsd?, concurrency?, autoConcurrency?, modelConcurrency?, models? }`. Any other key
/// (a tag, `pollIntervalSec`, `local`, `cursorModels`, engines) is refused when parsing.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AgentSettings {
    pub daily_usd: Option<f64>,
    pub concurrency: Option<f64>,
    pub auto_concurrency: Option<AutoChange>,
    pub model_concurrency: Option<ModelCaps>,
    pub models: Option<ModelsChange>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutoChange {
    pub enabled: Option<bool>,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelCaps {
    pub opus: Option<f64>,
    pub sonnet: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ModelsChange {
    pub opus_above_pct: Option<f64>,
    pub sonnet_above_pct: Option<f64>,
    pub pause_below_pct: Option<f64>,
}

impl AgentSettings {
    /// Parses a command's body; a key that isn't an agent setting is an error.
    pub fn parse(body: &Value) -> Result<Self> {
        serde_json::from_value(body.clone()).map_err(|e| anyhow!("Not an agent setting: {e}"))
    }
}

fn sessions(name: &str, v: f64) -> Result<u32> {
    if v.fract() != 0.0 || !(1.0..=64.0).contains(&v) {
        return Err(anyhow!("{name} must be a whole number from 1 to 64."));
    }
    Ok(v as u32)
}

fn pct(name: &str, v: f64) -> Result<u32> {
    if v.fract() != 0.0 || !(0.0..=100.0).contains(&v) {
        return Err(anyhow!("{name} must be a whole number from 0 to 100."));
    }
    Ok(v as u32)
}

fn current(obj: &Map<String, Value>, group: &str, key: &str) -> Option<u32> {
    obj.get(group)?.get(key)?.as_u64().and_then(|n| u32::try_from(n).ok())
}

/// Checks `change` and patches `obj` (the top level of `config.json`) with it. Nothing is changed when it is
/// refused: "Name a change." for an empty one, a field out of range, thresholds out of order, or auto's least
/// above its most.
pub fn with_agent_settings(obj: &mut Map<String, Value>, change: &AgentSettings) -> Result<()> {
    let mut patch: Vec<(&str, Option<&str>, Value)> = Vec::new();
    if let Some(usd) = change.daily_usd {
        if !(0.0..=100_000.0).contains(&usd) {
            return Err(anyhow!("Daily budget must be from 0 to 100000."));
        }
        patch.push(("budget", Some("dailyUsd"), json!(usd)));
    }
    if let Some(n) = change.concurrency {
        patch.push(("concurrency", None, json!(sessions("Sessions at once", n)?)));
    }
    if let Some(a) = &change.auto_concurrency {
        if let Some(on) = a.enabled {
            patch.push(("autoConcurrency", Some("enabled"), json!(on)));
        }
        let min = a.min.map(|v| sessions("Auto's least", v)).transpose()?;
        let max = a.max.map(|v| sessions("Auto's most", v)).transpose()?;
        let least = min.or_else(|| current(obj, "autoConcurrency", "min")).unwrap_or(1);
        let most = max.or_else(|| current(obj, "autoConcurrency", "max")).unwrap_or(8);
        if (min.is_some() || max.is_some()) && least > most {
            return Err(anyhow!("Auto's least can't be above its most."));
        }
        for (key, v) in [("min", min), ("max", max)] {
            if let Some(v) = v {
                patch.push(("autoConcurrency", Some(key), json!(v)));
            }
        }
    }
    if let Some(m) = &change.model_concurrency {
        for (key, v, name) in [
            ("opus", m.opus, "Opus sessions"),
            ("sonnet", m.sonnet, "Sonnet sessions"),
        ] {
            if let Some(v) = v {
                patch.push(("modelConcurrency", Some(key), json!(sessions(name, v)?)));
            }
        }
    }
    if let Some(m) = &change.models {
        let fields = [
            ("opusAbovePct", m.opus_above_pct, "Opus threshold", 50),
            ("sonnetAbovePct", m.sonnet_above_pct, "Sonnet threshold", 20),
            ("pauseBelowPct", m.pause_below_pct, "Pause threshold", 5),
        ];
        let mut now = [0u32; 3];
        for (i, (key, v, name, default)) in fields.iter().enumerate() {
            let new = v.map(|v| pct(name, v)).transpose()?;
            now[i] = new.or_else(|| current(obj, "models", key)).unwrap_or(*default);
            if let Some(new) = new {
                patch.push(("models", Some(key), json!(new)));
            }
        }
        if m.opus_above_pct.is_some() || m.sonnet_above_pct.is_some() || m.pause_below_pct.is_some() {
            let [opus, sonnet, pause] = now;
            if !(pause < sonnet && sonnet < opus) {
                return Err(anyhow!("Thresholds must go: pause below < Sonnet above < Opus above."));
            }
        }
    }
    if patch.is_empty() {
        return Err(anyhow!("Name a change."));
    }
    for (key, inner, value) in patch {
        match inner {
            None => {
                obj.insert(key.to_string(), value);
            }
            Some(inner) => {
                let slot = obj.entry(key.to_string()).or_insert_with(|| json!({}));
                if !slot.is_object() {
                    *slot = json!({});
                }
                slot.as_object_mut()
                    .expect("just made an object")
                    .insert(inner.to_string(), value);
            }
        }
    }
    Ok(())
}

/// Saves `change` to `<dir>/config.json` through [`config::save_machine_settings`]; a refused change writes
/// nothing new (the file is rewritten as it was).
pub fn save_agent_settings(dir: &Path, change: &AgentSettings) -> Result<()> {
    let mut refused = None;
    config::save_machine_settings(dir, |obj| {
        refused = with_agent_settings(obj, change).err();
    })?;
    refused.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests;
