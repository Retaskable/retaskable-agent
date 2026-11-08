//! `autoConcurrency` and `modelConcurrency` (agent/07.1-model-tiers.md), read leniently: a field that isn't
//! usable falls back to its default rather than failing the config load.
use serde::{Deserialize, Deserializer};
use serde_json::Value;

/// Auto concurrency: on by default, between `min` (default 1) and `max` (default 8) sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutoConcurrency {
    pub enabled: bool,
    pub min: u32,
    pub max: u32,
    /// Utilization (%) of the 5-hour window the pace aims for at the reset.
    pub target_pct: u32,
    /// Points the week may run ahead of an even pace before one session is dropped.
    pub week_margin_pct: u32,
    /// Fewest sessions the week's throttle leaves (or `max`, if lower).
    pub week_floor: u32,
}

impl Default for AutoConcurrency {
    fn default() -> Self {
        Self {
            enabled: true,
            min: 1,
            max: 8,
            target_pct: 75,
            week_margin_pct: 10,
            week_floor: 2,
        }
    }
}

/// Per-model session caps; `haiku` has none and `local` is always 1. `None` is no cap of its own.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ModelConcurrency {
    pub opus: Option<u32>,
    pub sonnet: Option<u32>,
}

fn pct(v: Option<&Value>) -> Option<u32> {
    v.and_then(Value::as_u64)
        .filter(|n| *n <= 100)
        .and_then(|n| u32::try_from(n).ok())
}

fn whole(v: Option<&Value>) -> Option<u32> {
    v.and_then(Value::as_u64)
        .filter(|n| *n > 0)
        .and_then(|n| u32::try_from(n).ok())
}

impl<'de> Deserialize<'de> for AutoConcurrency {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = Value::deserialize(d)?;
        let def = Self::default();
        let mut out = Self {
            enabled: raw.get("enabled").and_then(Value::as_bool).unwrap_or(def.enabled),
            min: whole(raw.get("min")).unwrap_or(def.min),
            max: whole(raw.get("max")).unwrap_or(def.max),
            target_pct: pct(raw.get("targetPct")).unwrap_or(def.target_pct),
            week_margin_pct: pct(raw.get("weekMarginPct")).unwrap_or(def.week_margin_pct),
            week_floor: whole(raw.get("weekFloor")).unwrap_or(def.week_floor),
        };
        if out.min > out.max {
            out.min = def.min.min(out.max);
        }
        Ok(out)
    }
}

impl<'de> Deserialize<'de> for ModelConcurrency {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = Value::deserialize(d)?;
        Ok(Self {
            opus: whole(raw.get("opus")),
            sonnet: whole(raw.get("sonnet")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MachineConfig;
    use serde_json::json;

    fn read(v: Value) -> MachineConfig {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn defaults_when_missing() {
        let cfg = read(json!({}));
        assert_eq!(cfg.auto_concurrency, AutoConcurrency::default());
        assert_eq!(cfg.model_concurrency, ModelConcurrency::default());
    }

    #[test]
    fn values_are_read() {
        let cfg = read(json!({ "autoConcurrency": { "enabled": false, "min": 2, "max": 5 },
            "modelConcurrency": { "opus": 1, "sonnet": 3 } }));
        assert_eq!(
            cfg.auto_concurrency,
            AutoConcurrency {
                enabled: false,
                min: 2,
                max: 5,
                ..AutoConcurrency::default()
            }
        );
        assert_eq!(
            cfg.model_concurrency,
            ModelConcurrency {
                opus: Some(1),
                sonnet: Some(3)
            }
        );
    }

    #[test]
    fn bad_values_fall_back_without_failing_the_load() {
        let cfg = read(json!({ "autoConcurrency": { "enabled": "yes", "min": 0, "max": -1 },
            "modelConcurrency": { "opus": "x", "sonnet": 0, "haiku": 2 } }));
        assert_eq!(cfg.auto_concurrency, AutoConcurrency::default());
        assert_eq!(cfg.model_concurrency, ModelConcurrency::default());
        let cfg = read(json!({ "autoConcurrency": 7, "modelConcurrency": [] }));
        assert_eq!(cfg.auto_concurrency, AutoConcurrency::default());
        let cfg = read(json!({ "autoConcurrency": { "min": 6, "max": 3 } }));
        assert_eq!(
            cfg.auto_concurrency,
            AutoConcurrency {
                max: 3,
                ..AutoConcurrency::default()
            }
        );
    }

    #[test]
    fn pacing_fields_are_read_with_defaults_and_bad_values() {
        let cfg = read(json!({ "autoConcurrency": { "targetPct": 60, "weekMarginPct": 0, "weekFloor": 3 } }));
        let a = cfg.auto_concurrency;
        assert_eq!((a.target_pct, a.week_margin_pct, a.week_floor), (60, 0, 3));
        let cfg = read(json!({ "autoConcurrency": { "targetPct": 150, "weekMarginPct": -1, "weekFloor": 0 } }));
        let a = cfg.auto_concurrency;
        assert_eq!((a.target_pct, a.week_margin_pct, a.week_floor), (75, 10, 2));
        let cfg = read(json!({ "autoConcurrency": { "targetPct": "x", "weekFloor": 1.5 } }));
        assert_eq!(cfg.auto_concurrency, AutoConcurrency::default());
    }

    #[test]
    fn config_load_reads_them() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            r#"{ "version": 2, "projects": [], "autoConcurrency": { "max": 4 }, "modelConcurrency": { "opus": 2 } }"#,
        )
        .unwrap();
        let cfg = crate::config::load(dir.path()).unwrap();
        assert_eq!(cfg.machine.auto_concurrency.max, 4);
        assert_eq!(cfg.machine.model_concurrency.opus, Some(2));
    }
}
