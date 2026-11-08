use super::*;
use serde_json::json;

fn change(v: Value) -> AgentSettings {
    AgentSettings::parse(&v).unwrap()
}

fn apply(obj: &mut Map<String, Value>, v: Value) -> Result<()> {
    with_agent_settings(obj, &change(v))
}

fn base() -> Map<String, Value> {
    json!({ "version": 2, "projects": [], "port": 4700, "pollIntervalSec": 10, "concurrency": 2,
        "budget": { "dailyUsd": 5, "plannerUsd": 1.5 }, "models": { "opusAbovePct": 50, "sonnetAbovePct": 20, "pauseBelowPct": 5 } })
    .as_object()
    .unwrap()
    .clone()
}

fn refused(v: Value) -> String {
    let mut obj = base();
    let before = obj.clone();
    let err = apply(&mut obj, v).unwrap_err().to_string();
    assert_eq!(obj, before, "a refused change touches nothing");
    err
}

#[test]
fn each_field_is_saved_and_the_rest_stays() {
    let mut obj = base();
    apply(&mut obj, json!({ "dailyUsd": 12.5 })).unwrap();
    assert_eq!(obj["budget"], json!({ "dailyUsd": 12.5, "plannerUsd": 1.5 }));
    apply(&mut obj, json!({ "concurrency": 4 })).unwrap();
    assert_eq!(obj["concurrency"], 4);
    apply(
        &mut obj,
        json!({ "autoConcurrency": { "enabled": false, "min": 2, "max": 6 } }),
    )
    .unwrap();
    assert_eq!(obj["autoConcurrency"], json!({ "enabled": false, "min": 2, "max": 6 }));
    apply(&mut obj, json!({ "modelConcurrency": { "opus": 1 } })).unwrap();
    apply(&mut obj, json!({ "modelConcurrency": { "sonnet": 3 } })).unwrap();
    assert_eq!(obj["modelConcurrency"], json!({ "opus": 1, "sonnet": 3 }));
    apply(&mut obj, json!({ "models": { "opusAbovePct": 60 } })).unwrap();
    assert_eq!(
        obj["models"],
        json!({ "opusAbovePct": 60, "sonnetAbovePct": 20, "pauseBelowPct": 5 })
    );
    assert_eq!(obj["port"], 4700);
    assert_eq!(obj["pollIntervalSec"], 10);
    assert_eq!(obj["projects"], json!([]));
    assert_eq!(obj["version"], 2);
}

#[test]
fn range_edges() {
    let ok = |v: Value| apply(&mut base(), v).is_ok();
    assert!(ok(json!({ "dailyUsd": 0 })));
    assert!(ok(json!({ "dailyUsd": 100000 })));
    assert!(!ok(json!({ "dailyUsd": -0.01 })));
    assert!(!ok(json!({ "dailyUsd": 100000.01 })));
    assert!(ok(json!({ "concurrency": 1 })));
    assert!(ok(json!({ "concurrency": 64 })));
    assert!(!ok(json!({ "concurrency": 0 })));
    assert!(!ok(json!({ "concurrency": 65 })));
    assert!(!ok(json!({ "concurrency": 2.5 })));
    assert!(ok(json!({ "modelConcurrency": { "opus": 64, "sonnet": 1 } })));
    assert!(!ok(json!({ "modelConcurrency": { "opus": 0 } })));
    assert!(!ok(json!({ "modelConcurrency": { "sonnet": 65 } })));
    assert!(ok(json!({ "autoConcurrency": { "min": 1, "max": 64 } })));
    assert!(!ok(json!({ "autoConcurrency": { "max": 65 } })));
    assert!(!ok(json!({ "autoConcurrency": { "min": 0 } })));
    assert!(ok(
        json!({ "models": { "pauseBelowPct": 0, "sonnetAbovePct": 1, "opusAbovePct": 100 } })
    ));
    assert!(!ok(json!({ "models": { "opusAbovePct": 101 } })));
    assert!(!ok(json!({ "models": { "pauseBelowPct": -1 } })));
    assert!(!ok(json!({ "models": { "pauseBelowPct": 1.5 } })));
}

#[test]
fn an_empty_change_is_refused_in_setups_words() {
    assert_eq!(refused(json!({})), "Name a change.");
    assert_eq!(refused(json!({ "autoConcurrency": {} })), "Name a change.");
    assert_eq!(refused(json!({ "models": {} })), "Name a change.");
}

#[test]
fn thresholds_out_of_order_are_refused() {
    assert!(refused(json!({ "models": { "opusAbovePct": 10 } })).contains("Thresholds"));
    assert!(refused(json!({ "models": { "pauseBelowPct": 20 } })).contains("Thresholds"));
    assert!(refused(json!({ "models": { "sonnetAbovePct": 60, "opusAbovePct": 50 } })).contains("Thresholds"));
}

#[test]
fn autos_least_above_its_most_is_refused() {
    assert!(refused(json!({ "autoConcurrency": { "min": 5, "max": 3 } })).contains("least"));
    // Against the saved side: the default most is 8.
    assert!(refused(json!({ "autoConcurrency": { "min": 9 } })).contains("least"));
    let mut obj = base();
    apply(&mut obj, json!({ "autoConcurrency": { "min": 3, "max": 4 } })).unwrap();
    assert!(apply(&mut obj, json!({ "autoConcurrency": { "max": 2 } })).is_err());
}

#[test]
fn computer_only_keys_are_refused() {
    for key in ["pollIntervalSec", "local", "tags", "cursorModels", "engines", "port"] {
        assert!(AgentSettings::parse(&json!({ key: 5 })).is_err(), "{key}");
    }
    assert!(AgentSettings::parse(&json!({ "dailyUsd": 1, "tags": [] })).is_err());
}

#[test]
fn saving_writes_through_config_and_keeps_the_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.json"),
        r#"{ "version": 2, "projects": [], "pollIntervalSec": 30, "custom": true }"#,
    )
    .unwrap();
    save_agent_settings(dir.path(), &change(json!({ "concurrency": 3, "dailyUsd": 7 }))).unwrap();
    let text = std::fs::read_to_string(dir.path().join("config.json")).unwrap();
    let saved: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(saved["concurrency"], 3);
    assert_eq!(saved["budget"]["dailyUsd"], 7.0);
    assert_eq!(saved["pollIntervalSec"], 30);
    assert_eq!(saved["custom"], true);
    let cfg = config::load(dir.path()).unwrap();
    assert_eq!(cfg.machine.concurrency, 3);
    assert_eq!(cfg.machine.budget.daily_usd, 7.0);
    assert!(save_agent_settings(dir.path(), &AgentSettings::default()).is_err());
}
