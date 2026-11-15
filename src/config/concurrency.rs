//! `modelConcurrency` (agent/07.1-model-tiers.md), read leniently: a field that isn't
//! usable falls back to its default rather than failing the config load.
use serde::{Deserialize, Deserializer};
use serde_json::Value;

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
        assert_eq!(cfg.model_concurrency, ModelConcurrency::default());
    }

    #[test]
    fn values_are_read() {
        let cfg = read(json!({ "autoConcurrency": { "max": 5 },
            "modelConcurrency": { "opus": 1, "sonnet": 3 } }));
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
        let cfg = read(json!({ "modelConcurrency": { "opus": "x", "sonnet": 0, "haiku": 2 } }));
        assert_eq!(cfg.model_concurrency, ModelConcurrency::default());
        let cfg = read(json!({ "modelConcurrency": [] }));
        assert_eq!(cfg.model_concurrency, ModelConcurrency::default());
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
        assert_eq!(cfg.machine.model_concurrency.opus, Some(2));
    }
}
