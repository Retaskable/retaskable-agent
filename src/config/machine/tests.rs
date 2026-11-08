use super::*;

fn with_port(port: u16) -> ConfigFile {
    ConfigFile {
        machine: MachineConfig {
            port: Some(port),
            ..MachineConfig::default()
        },
        ..ConfigFile::default()
    }
}

#[test]
fn lane_scaling_and_the_chore_lane_are_read_and_a_bad_one_is_dropped_not_fatal() {
    let read = |v: Value| serde_json::from_value::<MachineConfig>(v).unwrap();
    let cfg = read(serde_json::json!({ "lanes": 3, "choreLane": 3, "laneScaling": { "base": 1, "belowUsedPct": 80 } }));
    assert_eq!(cfg.chore_lane, Some(3));
    assert_eq!(
        cfg.lane_scaling,
        Some(LaneScaling {
            base: 1,
            below_used_pct: 80.0
        })
    );
    let bad = read(serde_json::json!({ "lanes": 3, "choreLane": 0, "laneScaling": { "base": "one" } }));
    assert_eq!((bad.chore_lane, bad.lane_scaling, bad.lanes), (None, None, 3));
    let missing = read(serde_json::json!({}));
    assert_eq!((missing.chore_lane, missing.lane_scaling), (None, None));
}

#[test]
fn port_prefers_the_env_then_the_file_then_the_callers_default() {
    let env = Env::from_pairs([("RETASKABLE_PORT", "4800")]);
    assert_eq!(port(&with_port(4900), &env, DEFAULT_PORT), 4800);
    assert_eq!(port(&with_port(4900), &Env::empty(), DEFAULT_PORT), 4900);
    assert_eq!(port(&ConfigFile::default(), &Env::empty(), DEFAULT_PORT), DEFAULT_PORT);
    let bad = Env::from_pairs([("RETASKABLE_PORT", "nope")]);
    assert_eq!(port(&ConfigFile::default(), &bad, DEFAULT_PORT), DEFAULT_PORT);
    // An unparseable env value falls through to the file, not straight to the default.
    assert_eq!(port(&with_port(4900), &bad, DEFAULT_PORT), 4900);
}

#[test]
fn with_no_port_in_the_file_either_it_falls_back_to_the_callers_default() {
    // installers/03-the-bundle-agent-bundle.md section 3.13: the final fallback is this
    // build's own default (a Staging release build's 4701), not a hardcoded 4700.
    assert_eq!(port(&ConfigFile::default(), &Env::empty(), 4701), 4701);
}

#[test]
fn a_zero_env_port_is_not_used_either() {
    let zero = Env::from_pairs([("RETASKABLE_PORT", "0")]);
    assert_eq!(port(&with_port(4900), &zero, DEFAULT_PORT), 4900);
    assert_eq!(port(&ConfigFile::default(), &zero, DEFAULT_PORT), DEFAULT_PORT);
}

#[test]
fn a_zero_port_in_the_file_is_not_used_either() {
    // Binding port 0 asks the OS for a random ephemeral port, not the documented
    // fallback, so a zero in config.json is as invalid as a zero env value.
    assert_eq!(port(&with_port(0), &Env::empty(), DEFAULT_PORT), DEFAULT_PORT);
}

#[test]
fn a_privileged_port_below_1024_is_not_used_either() {
    // agent-settings.md section 3.1: the documented valid range is 1024 to 65535,
    // so a sub-1024 port (env or file) is as invalid as zero and falls through too.
    let low_env = Env::from_pairs([("RETASKABLE_PORT", "80")]);
    assert_eq!(port(&with_port(4900), &low_env, DEFAULT_PORT), 4900);
    assert_eq!(port(&ConfigFile::default(), &low_env, DEFAULT_PORT), DEFAULT_PORT);
    assert_eq!(port(&with_port(80), &Env::empty(), DEFAULT_PORT), DEFAULT_PORT);
    assert_eq!(port(&with_port(1024), &Env::empty(), DEFAULT_PORT), 1024);
}

#[test]
fn a_port_at_the_top_of_the_documented_range_is_used() {
    // agent-settings.md section 3.1: the documented valid range runs up to 65535,
    // so the top of the range is as valid as the bottom (already covered at 1024).
    let top_env = Env::from_pairs([("RETASKABLE_PORT", "65535")]);
    assert_eq!(port(&ConfigFile::default(), &top_env, DEFAULT_PORT), 65535);
    assert_eq!(port(&with_port(65535), &Env::empty(), DEFAULT_PORT), 65535);
}

#[test]
fn poll_interval_sec_prefers_the_env_then_the_file_then_the_default() {
    let env = Env::from_pairs([("RETASKABLE_POLL_SEC", "30")]);
    let mut cfg = ConfigFile::default();
    cfg.machine.poll_interval_sec = 20;
    assert_eq!(poll_interval_sec(&cfg, &env), 30);
    assert_eq!(poll_interval_sec(&cfg, &Env::empty()), 20);
    assert_eq!(poll_interval_sec(&ConfigFile::default(), &Env::empty()), 10);
}

#[test]
fn poll_interval_sec_outside_2_to_3600_falls_through_to_the_file() {
    let mut cfg = ConfigFile::default();
    cfg.machine.poll_interval_sec = 20;
    let too_low = Env::from_pairs([("RETASKABLE_POLL_SEC", "1")]);
    let too_high = Env::from_pairs([("RETASKABLE_POLL_SEC", "3601")]);
    let not_a_number = Env::from_pairs([("RETASKABLE_POLL_SEC", "soon")]);
    assert_eq!(poll_interval_sec(&cfg, &too_low), 20);
    assert_eq!(poll_interval_sec(&cfg, &too_high), 20);
    assert_eq!(poll_interval_sec(&cfg, &not_a_number), 20);
    // The documented range's ends are both valid.
    let low_end = Env::from_pairs([("RETASKABLE_POLL_SEC", "2")]);
    let high_end = Env::from_pairs([("RETASKABLE_POLL_SEC", "3600")]);
    assert_eq!(poll_interval_sec(&cfg, &low_end), 2);
    assert_eq!(poll_interval_sec(&cfg, &high_end), 3600);
}

#[test]
fn ci_slots_prefers_the_env_then_the_file_then_the_default_of_1() {
    let mut cfg = ConfigFile::default();
    cfg.machine.ci_slots = 3;
    assert_eq!(ci_slots(&cfg, &Env::from_pairs([("RETASKABLE_CI_SLOTS", "5")])), 5);
    assert_eq!(ci_slots(&cfg, &Env::empty()), 3);
    assert_eq!(ci_slots(&ConfigFile::default(), &Env::empty()), 1);
}

#[test]
fn ci_slots_accepts_0_to_8_and_anything_else_falls_through() {
    let mut cfg = ConfigFile::default();
    cfg.machine.ci_slots = 2;
    for (v, want) in [("0", 0), ("8", 8), ("-1", 2), ("9", 2), ("two", 2), ("1.5", 2), ("", 2)] {
        assert_eq!(
            ci_slots(&cfg, &Env::from_pairs([("RETASKABLE_CI_SLOTS", v)])),
            want,
            "{v}"
        );
    }
    cfg.machine.ci_slots = 9;
    assert_eq!(ci_slots(&cfg, &Env::empty()), 1);
}

#[test]
fn lanes_prefers_the_env_then_the_file_then_the_default() {
    let env = Env::from_pairs([("RETASKABLE_LANES", "8")]);
    let mut cfg = ConfigFile::default();
    cfg.machine.lanes = 4;
    assert_eq!(lanes(&cfg, &env), 8);
    assert_eq!(lanes(&cfg, &Env::empty()), 4);
    assert_eq!(lanes(&ConfigFile::default(), &Env::empty()), 2);
}

#[test]
fn lanes_outside_1_to_64_falls_through_to_the_file() {
    let mut cfg = ConfigFile::default();
    cfg.machine.lanes = 4;
    let too_low = Env::from_pairs([("RETASKABLE_LANES", "0")]);
    let too_high = Env::from_pairs([("RETASKABLE_LANES", "65")]);
    assert_eq!(lanes(&cfg, &too_low), 4);
    assert_eq!(lanes(&cfg, &too_high), 4);
    let low_end = Env::from_pairs([("RETASKABLE_LANES", "1")]);
    let high_end = Env::from_pairs([("RETASKABLE_LANES", "64")]);
    assert_eq!(lanes(&cfg, &low_end), 1);
    assert_eq!(lanes(&cfg, &high_end), 64);
}

#[test]
fn local_model_env_turns_it_on_with_that_models_name() {
    let env = Env::from_pairs([("RETASKABLE_LOCAL_MODEL", "llama3.1")]);
    let result = local_model(&ConfigFile::default(), &env);
    assert!(result.enabled);
    assert_eq!(result.model, "llama3.1");
    // The file's own address is kept; RETASKABLE_LOCAL_URL wasn't set.
    assert_eq!(result.base_url, default_local_base_url());
}

#[test]
fn local_model_env_off_values_turn_it_off() {
    let mut cfg = ConfigFile::default();
    cfg.machine.local.enabled = true;
    cfg.machine.local.model = "llama3.1".to_string();
    for off in ["off", "false", "0", "no", "OFF"] {
        let env = Env::from_pairs([("RETASKABLE_LOCAL_MODEL", off)]);
        assert!(!local_model(&cfg, &env).enabled, "{off} should turn it off");
    }
}

#[test]
fn local_model_env_falls_through_to_the_file_when_invalid() {
    let mut cfg = ConfigFile::default();
    cfg.machine.local.enabled = true;
    cfg.machine.local.model = "llama3.1".to_string();
    // Over 100 characters, and a character outside the documented set.
    let too_long = "a".repeat(101);
    let env = Env::from_pairs([("RETASKABLE_LOCAL_MODEL", too_long.clone())]);
    let result = local_model(&cfg, &env);
    assert!(result.enabled);
    assert_eq!(result.model, "llama3.1");
    let bad_char = Env::from_pairs([("RETASKABLE_LOCAL_MODEL", "model name")]);
    assert_eq!(local_model(&cfg, &bad_char).model, "llama3.1");
}

#[test]
fn local_model_url_env_sets_its_address() {
    let env = Env::from_pairs([("RETASKABLE_LOCAL_URL", "http://10.0.0.5:11436")]);
    assert_eq!(
        local_model(&ConfigFile::default(), &env).base_url,
        "http://10.0.0.5:11436"
    );
}

#[test]
fn local_model_url_env_with_no_scheme_or_a_user_falls_through_to_the_file() {
    let cfg = ConfigFile::default();
    let no_scheme = Env::from_pairs([("RETASKABLE_LOCAL_URL", "10.0.0.5:11436")]);
    let with_user = Env::from_pairs([("RETASKABLE_LOCAL_URL", "http://me:pw@10.0.0.5:11436")]);
    assert_eq!(local_model(&cfg, &no_scheme).base_url, default_local_base_url());
    assert_eq!(local_model(&cfg, &with_user).base_url, default_local_base_url());
}

#[test]
fn engine_choice_missing_is_empty() {
    let result = engine_choice(&ConfigFile::default(), &Env::empty());
    assert_eq!(result.default, None);
    assert_eq!(result.roles, RoleEngines::default());
    assert!(result.lanes.is_empty());
}

#[test]
fn engine_choice_file_values_are_used_with_no_env() {
    let mut cfg = ConfigFile::default();
    cfg.machine.engine_choice.default = Some(Engine::Claude);
    cfg.machine.engine_choice.roles.implementer = Some(Engine::Local);
    cfg.machine.engine_choice.lanes = vec![LaneEngine::Claude, LaneEngine::Cursor];
    let result = engine_choice(&cfg, &Env::empty());
    assert_eq!(result.default, Some(Engine::Claude));
    assert_eq!(result.roles.implementer, Some(Engine::Local));
    assert_eq!(result.lanes, vec![LaneEngine::Claude, LaneEngine::Cursor]);
}

#[test]
fn engine_choice_env_a_bare_engine_replaces_the_files_whole_engine_choice() {
    let mut cfg = ConfigFile::default();
    cfg.machine.engine_choice.roles.implementer = Some(Engine::Cursor);
    let env = Env::from_pairs([("RETASKABLE_ENGINES", "local")]);
    let result = engine_choice(&cfg, &env);
    assert_eq!(result.default, Some(Engine::Local));
    // The variable replaces the file's whole engineChoice, not just the default: the file's
    // role override is gone (TypeScript's withEnvSettings: `next.engineChoice = e.engines`).
    assert_eq!(result.roles.implementer, None);
}

#[test]
fn engine_choice_env_a_bare_unknown_value_still_replaces_the_file_with_empty() {
    // `{}` is truthy in the TypeScript agent, so even a value that parses to nothing still
    // replaces the file's engineChoice wholesale rather than leaving it alone.
    let mut cfg = ConfigFile::default();
    cfg.machine.engine_choice.default = Some(Engine::Cursor);
    cfg.machine.engine_choice.roles.implementer = Some(Engine::Local);
    let env = Env::from_pairs([("RETASKABLE_ENGINES", "steam")]);
    assert_eq!(engine_choice(&cfg, &env), EngineChoiceConfig::default());
}

#[test]
fn engine_choice_env_pairs_set_default_and_roles() {
    let env = Env::from_pairs([("RETASKABLE_ENGINES", "default=cursor,implementer=local")]);
    let result = engine_choice(&ConfigFile::default(), &env);
    assert_eq!(result.default, Some(Engine::Cursor));
    assert_eq!(result.roles.implementer, Some(Engine::Local));
    assert_eq!(result.roles.planner, None);
}

#[test]
fn engine_choice_env_an_invalid_role_value_is_left_out_of_the_replacement() {
    let mut cfg = ConfigFile::default();
    cfg.machine.engine_choice.roles.planner = Some(Engine::Cursor);
    let env = Env::from_pairs([("RETASKABLE_ENGINES", "planner=steam")]);
    // The variable still replaces the file's engineChoice, so the invalid value doesn't fall
    // through to the file's role: the whole thing ends up unset.
    assert_eq!(engine_choice(&cfg, &env).roles.planner, None);
}

#[test]
fn engine_choice_env_lanes_sets_them_in_order() {
    let env = Env::from_pairs([("RETASKABLE_ENGINES", "default=claude,lanes=claude+claude+cursor+cursor")]);
    let result = engine_choice(&ConfigFile::default(), &env);
    assert_eq!(
        result.lanes,
        vec![
            LaneEngine::Claude,
            LaneEngine::Claude,
            LaneEngine::Cursor,
            LaneEngine::Cursor
        ]
    );
}

#[test]
fn engine_choice_env_lanes_with_any_invalid_entry_is_left_out_whole() {
    let mut cfg = ConfigFile::default();
    cfg.machine.engine_choice.lanes = vec![LaneEngine::Cursor, LaneEngine::Cursor];
    let env = Env::from_pairs([("RETASKABLE_ENGINES", "lanes=claude+steam+cursor")]);
    // One bad entry drops the whole list; since the variable replaces the file's engineChoice
    // entirely, the result is empty, not the file's lanes.
    assert_eq!(engine_choice(&cfg, &env).lanes, Vec::<LaneEngine>::new());
}

#[test]
fn engine_choice_env_lanes_with_nothing_after_it_is_an_empty_list() {
    let mut cfg = ConfigFile::default();
    cfg.machine.engine_choice.lanes = vec![LaneEngine::Cursor, LaneEngine::Cursor];
    let env = Env::from_pairs([("RETASKABLE_ENGINES", "lanes=")]);
    assert_eq!(engine_choice(&cfg, &env).lanes, Vec::<LaneEngine>::new());
}

#[test]
fn engine_choice_env_lanes_drops_trailing_default_entries() {
    let env = Env::from_pairs([("RETASKABLE_ENGINES", "lanes=claude+default")]);
    assert_eq!(
        engine_choice(&ConfigFile::default(), &env).lanes,
        vec![LaneEngine::Claude]
    );
}

#[test]
fn engine_choice_env_lanes_keeps_a_default_entry_that_isnt_trailing() {
    let env = Env::from_pairs([("RETASKABLE_ENGINES", "lanes=default+claude")]);
    assert_eq!(
        engine_choice(&ConfigFile::default(), &env).lanes,
        vec![LaneEngine::Default, LaneEngine::Claude]
    );
}

#[test]
fn engine_choice_and_cursor_models_read_from_json_the_way_the_file_spells_them() {
    let json = r#"{
        "engineChoice": {
            "default": "cursor",
            "roles": { "implementer": "local" },
            "lanes": ["claude", "claude", "cursor", "default"]
        },
        "cursor": { "models": { "top": "gpt-5", "implementer": "auto", "scout": "" } }
    }"#;
    let cfg: MachineConfig = serde_json::from_str(json).unwrap();
    assert_eq!(cfg.engine_choice.default, Some(Engine::Cursor));
    assert_eq!(cfg.engine_choice.roles.implementer, Some(Engine::Local));
    assert_eq!(cfg.engine_choice.roles.planner, None);
    // The file's lanes go through the same lenient reading as the environment's: a trailing
    // "default" entry is dropped (lanesOf).
    assert_eq!(
        cfg.engine_choice.lanes,
        vec![LaneEngine::Claude, LaneEngine::Claude, LaneEngine::Cursor]
    );
    let resolved = cursor_models(
        &ConfigFile {
            machine: cfg,
            ..ConfigFile::default()
        },
        &Env::empty(),
    );
    assert_eq!(resolved.top, Some("gpt-5".to_string()));
    assert_eq!(resolved.implementer, Some("auto".to_string()));
    // An empty string is no name at all, so it reads as auto (None).
    assert_eq!(resolved.scout, None);
}

#[test]
fn an_unknown_engine_in_the_file_reads_as_none_instead_of_failing() {
    // "pi" is a real engine (engines.md section 13): it must parse, case and all. "steam" is
    // not an engine at all, and a lane holding one must not fail the whole config load either.
    let json = r#"{
        "engineChoice": {
            "default": "PI",
            "roles": { "implementer": "steam" },
            "lanes": ["claude", "steam"]
        }
    }"#;
    let cfg: MachineConfig = serde_json::from_str(json).unwrap();
    assert_eq!(cfg.engine_choice.default, Some(Engine::Pi));
    assert_eq!(cfg.engine_choice.roles.implementer, None);
    // One bad lane entry drops the whole list rather than just that entry.
    assert!(cfg.engine_choice.lanes.is_empty());
}

#[test]
fn codex_is_an_engine_and_a_lane_pick() {
    let json = r#"{
        "engineChoice": {
            "default": "codex",
            "roles": { "review": "Codex" },
            "lanes": ["claude", "codex", "default"]
        }
    }"#;
    let cfg: MachineConfig = serde_json::from_str(json).unwrap();
    assert_eq!(cfg.engine_choice.default, Some(Engine::Codex));
    assert_eq!(cfg.engine_choice.roles.review, Some(Engine::Codex));
    assert_eq!(cfg.engine_choice.lanes, vec![LaneEngine::Claude, LaneEngine::Codex]);
    let env_lanes = lanes_of(&["codex".to_string(), "cursor".to_string()]);
    assert_eq!(env_lanes, Some(vec![LaneEngine::Codex, LaneEngine::Cursor]));
    assert_eq!(Engine::Codex.to_string(), "codex");
    assert_eq!(lanes_of(&["claude".to_string(), "gemini".to_string()]), None);
}

#[test]
fn cursor_models_missing_is_auto() {
    let result = cursor_models(&ConfigFile::default(), &Env::empty());
    assert_eq!(result.top, None);
    assert_eq!(result.implementer, None);
    assert_eq!(result.scout, None);
}

#[test]
fn cursor_models_a_bad_name_in_the_file_reads_as_auto() {
    let mut cfg = ConfigFile::default();
    cfg.machine.cursor.models.top = Some("--force".to_string());
    assert_eq!(cursor_models(&cfg, &Env::empty()).top, None);
}

#[test]
fn cursor_models_env_a_bare_name_sets_every_tier() {
    let env = Env::from_pairs([("RETASKABLE_CURSOR_MODELS", "gpt-5")]);
    let result = cursor_models(&ConfigFile::default(), &env);
    assert_eq!(result.top, Some("gpt-5".to_string()));
    assert_eq!(result.implementer, Some("gpt-5".to_string()));
    assert_eq!(result.scout, Some("gpt-5".to_string()));
}

#[test]
fn cursor_models_env_pairs_set_each_tier_by_name() {
    let env = Env::from_pairs([(
        "RETASKABLE_CURSOR_MODELS",
        "top=gpt-5,implementer=sonnet,scout=gpt-5-mini",
    )]);
    let result = cursor_models(&ConfigFile::default(), &env);
    assert_eq!(result.top, Some("gpt-5".to_string()));
    assert_eq!(result.implementer, Some("sonnet".to_string()));
    assert_eq!(result.scout, Some("gpt-5-mini".to_string()));
}

#[test]
fn cursor_models_env_an_invalid_name_falls_through_to_the_file() {
    let mut cfg = ConfigFile::default();
    cfg.machine.cursor.models.top = Some("sonnet".to_string());
    let bare = Env::from_pairs([("RETASKABLE_CURSOR_MODELS", "a b")]);
    assert_eq!(cursor_models(&cfg, &bare).top, Some("sonnet".to_string()));
    let pair = Env::from_pairs([("RETASKABLE_CURSOR_MODELS", "top=--force")]);
    assert_eq!(cursor_models(&cfg, &pair).top, Some("sonnet".to_string()));
}
