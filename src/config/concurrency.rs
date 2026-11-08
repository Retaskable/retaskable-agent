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
}

impl Default for AutoConcurrency {
    fn default() -> Self {
        Self {
            enabled: true,
            min: 1,
            max: 8,
        }
    }
}

/// Per-model session caps; `haiku` has none and `local` is always 1. `None` is no cap of its own.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ModelConcurrency {
    pub opus: Option<u32>,
    pub sonnet: Option<u32>,
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
        assert_eq!(
            cfg.auto_concurrency,
            AutoConcurrency {
                enabled: true,
                min: 1,
                max: 8
            }
        );
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
                max: 5
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
                enabled: true,
                min: 1,
                max: 3
            }
        );
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
