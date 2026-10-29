use std::path::Path;

use serde_json::json;

use super::*;

fn base_spec(cwd: &Path) -> SessionSpec {
    crate::engines::engine::test_spec(cwd, "do the thing")
}

// build_args

#[test]
fn build_args_has_the_documented_flags() {
    let dir = tempfile::tempdir().unwrap();
    let args = build_args(&base_spec(dir.path()));
    assert_eq!(args[0], "-p");
    assert_eq!(args[1], "--model");
    assert_eq!(args[2], "sonnet");
    for flag in [
        "--permission-mode",
        "--output-format",
        "--verbose",
        "--strict-mcp-config",
        "--setting-sources",
        "--no-session-persistence",
        "--disallowedTools",
    ] {
        assert!(args.contains(&flag.to_string()), "missing {flag} in {args:?}");
    }
    for absent in ["--max-budget-usd", "--max-turns", "--mcp-config", "--session-id"] {
        assert!(!args.contains(&absent.to_string()), "unexpected {absent} in {args:?}");
    }
}

#[test]
fn build_args_never_omits_the_no_deploy_tools() {
    let dir = tempfile::tempdir().unwrap();
    let args = build_args(&base_spec(dir.path()));
    let idx = args.iter().position(|a| a == "--disallowedTools").unwrap();
    for tool in NO_DEPLOY_TOOLS {
        assert!(args[idx..].contains(&tool.to_string()));
    }
}

#[test]
fn build_args_adds_the_budget_cap_only_when_positive() {
    let dir = tempfile::tempdir().unwrap();
    let mut spec = base_spec(dir.path());
    spec.budget_usd = 5.0;
    let args = build_args(&spec);
    let idx = args.iter().position(|a| a == "--max-budget-usd").unwrap();
    assert_eq!(args[idx + 1], "5");
}

#[test]
fn build_args_never_caps_a_local_sessions_budget() {
    let dir = tempfile::tempdir().unwrap();
    let mut spec = local_spec(dir.path());
    spec.budget_usd = 5.0;
    assert!(!build_args(&spec).contains(&"--max-budget-usd".to_string()));
}

#[test]
fn build_args_adds_max_turns_when_set() {
    let dir = tempfile::tempdir().unwrap();
    let mut spec = base_spec(dir.path());
    spec.max_turns = Some(300);
    let args = build_args(&spec);
    let idx = args.iter().position(|a| a == "--max-turns").unwrap();
    assert_eq!(args[idx + 1], "300");
}

#[test]
fn build_args_points_at_the_projects_mcp_config_only_when_it_exists() {
    let dir = tempfile::tempdir().unwrap();
    let spec = base_spec(dir.path());
    assert!(!build_args(&spec).contains(&"--mcp-config".to_string()));
    std::fs::write(dir.path().join(".mcp.json"), "{}").unwrap();
    let args = build_args(&spec);
    let idx = args.iter().position(|a| a == "--mcp-config").unwrap();
    assert_eq!(args[idx + 1], dir.path().join(".mcp.json").to_string_lossy());
}

#[test]
fn build_args_starts_a_named_session_instead_of_no_persistence_when_fresh() {
    let dir = tempfile::tempdir().unwrap();
    let mut spec = base_spec(dir.path());
    spec.start = SessionStart::Fresh("abc-123".to_string());
    let args = build_args(&spec);
    assert!(!args.contains(&"--no-session-persistence".to_string()));
    assert!(!args.contains(&"--resume".to_string()));
    let idx = args.iter().position(|a| a == "--session-id").unwrap();
    assert_eq!(args[idx + 1], "abc-123");
}

#[test]
fn build_args_resumes_with_the_sessions_id_instead_of_no_persistence_or_a_fresh_one() {
    let dir = tempfile::tempdir().unwrap();
    let mut spec = base_spec(dir.path());
    spec.start = SessionStart::Resume("abc-123".to_string());
    let args = build_args(&spec);
    assert!(!args.contains(&"--no-session-persistence".to_string()));
    assert!(!args.contains(&"--session-id".to_string()));
    let idx = args.iter().position(|a| a == "--resume").unwrap();
    assert_eq!(args[idx + 1], "abc-123");
}

#[test]
fn build_args_keeps_every_other_flag_the_same_on_a_resume() {
    let dir = tempfile::tempdir().unwrap();
    let mut fresh = base_spec(dir.path());
    fresh.start = SessionStart::Fresh("abc-123".to_string());
    let mut resumed = base_spec(dir.path());
    resumed.start = SessionStart::Resume("abc-123".to_string());
    let without_start = |args: Vec<String>| -> Vec<String> {
        let idx = args
            .iter()
            .position(|a| ["--session-id", "--resume", "--no-session-persistence"].contains(&a.as_str()))
            .unwrap();
        let skip = if args[idx] == "--no-session-persistence" { 1 } else { 2 };
        [&args[..idx], &args[idx + skip..]].concat()
    };
    assert_eq!(without_start(build_args(&fresh)), without_start(build_args(&resumed)));
}

// session_env

fn spec_with_model(dir: &Path, model: &str) -> SessionSpec {
    let mut spec = base_spec(dir);
    spec.model = model.to_string();
    spec
}

fn subagent_model(dir: &Path, model: &str) -> String {
    session_env(&spec_with_model(dir, model))
        .into_iter()
        .find(|(k, _)| k == "CLAUDE_CODE_SUBAGENT_MODEL")
        .unwrap()
        .1
}

#[test]
fn session_env_runs_subagents_on_sonnet_by_default_and_haiku_for_a_haiku_session() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(subagent_model(dir.path(), "sonnet"), "sonnet");
    assert_eq!(subagent_model(dir.path(), "opus"), "sonnet");
    assert_eq!(subagent_model(dir.path(), "haiku"), "haiku");
}

#[test]
fn session_env_disables_claude_ai_connectors_and_sets_the_compact_window() {
    let dir = tempfile::tempdir().unwrap();
    let env = session_env(&base_spec(dir.path()));
    assert!(env.contains(&("ENABLE_CLAUDEAI_MCP_SERVERS".to_string(), "false".to_string())));
    assert!(env.contains(&("CLAUDE_CODE_AUTO_COMPACT_WINDOW".to_string(), "200000".to_string())));
}

#[test]
fn session_env_overwrites_the_cloudflare_token_so_nothing_can_deploy() {
    let dir = tempfile::tempdir().unwrap();
    let env = session_env(&base_spec(dir.path()));
    assert!(
        env.iter()
            .any(|(k, v)| k == "CLOUDFLARE_API_TOKEN" && v == NO_DEPLOY_TOKEN)
    );
    assert!(env.iter().any(|(k, v)| k == "CF_API_TOKEN" && v == NO_DEPLOY_TOKEN));
}

fn local_spec(dir: &Path) -> SessionSpec {
    let mut spec = base_spec(dir);
    spec.model = "local".to_string();
    spec.local = Some(LocalTarget {
        base_url: "http://localhost:11436".to_string(),
        model: "tiel-coder-35b".to_string(),
    });
    spec
}

#[test]
fn local_target_only_agrees_the_session_is_local_when_both_signals_do() {
    let dir = tempfile::tempdir().unwrap();
    // A `local` target carried over onto a session whose model isn't actually "local": not local.
    let mut mismatched_model = local_spec(dir.path());
    mismatched_model.model = "opus".to_string();
    assert!(local_target(&mismatched_model).is_none());
    // `model == "local"` with no server configured: not local either, since there's nowhere
    // for the session's environment to actually point.
    let mut no_target = local_spec(dir.path());
    no_target.local = None;
    assert!(local_target(&no_target).is_none());
    assert!(local_target(&local_spec(dir.path())).is_some());
}

#[test]
fn session_env_points_a_local_session_at_the_local_server_and_skips_cloud_only_vars() {
    let dir = tempfile::tempdir().unwrap();
    let env = session_env(&local_spec(dir.path()));
    assert!(env.contains(&("ANTHROPIC_BASE_URL".to_string(), "http://localhost:11436".to_string())));
    assert!(env.contains(&("ANTHROPIC_AUTH_TOKEN".to_string(), "local".to_string())));
    assert!(env.contains(&("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".to_string(), "1".to_string())));
    let model_vars = [
        "ANTHROPIC_DEFAULT_OPUS_MODEL",
        "ANTHROPIC_DEFAULT_SONNET_MODEL",
        "ANTHROPIC_DEFAULT_HAIKU_MODEL",
        "ANTHROPIC_SMALL_FAST_MODEL",
        "CLAUDE_CODE_SUBAGENT_MODEL",
    ];
    assert!(
        model_vars
            .iter()
            .all(|var| env.contains(&(var.to_string(), "tiel-coder-35b".to_string())))
    );
    assert!(env.contains(&("ENABLE_CLAUDEAI_MCP_SERVERS".to_string(), "false".to_string())));
    assert!(
        env.iter()
            .any(|(k, v)| k == "CLOUDFLARE_API_TOKEN" && v == NO_DEPLOY_TOKEN)
    );
    assert!(!env.iter().any(|(k, _)| k == "CLAUDE_CODE_AUTO_COMPACT_WINDOW"));
}

// sandbox_supported / sandbox_enabled / sandbox_settings / build_args sandbox

#[test]
fn sandbox_is_supported_on_every_platform_except_windows() {
    assert!(!sandbox_supported(true));
    assert!(sandbox_supported(false));
}

#[test]
fn sandbox_enabled_needs_both_auto_and_a_supporting_platform() {
    use crate::config::SandboxMode;
    assert!(sandbox_enabled(SandboxMode::Auto, false));
    assert!(!sandbox_enabled(SandboxMode::Auto, true));
    assert!(!sandbox_enabled(SandboxMode::Off, false));
    assert!(!sandbox_enabled(SandboxMode::Off, true));
}

#[test]
fn sandbox_settings_allows_only_the_macos_certificate_checker() {
    assert_eq!(
        sandbox_settings(false),
        json!({"enabled": true, "autoAllowBashIfSandboxed": true})
    );
    assert_eq!(
        sandbox_settings(true),
        json!({
            "enabled": true,
            "autoAllowBashIfSandboxed": true,
            "network": {"allowMachLookup": ["com.apple.trustd.agent"]},
        })
    );
}

#[test]
fn build_args_omits_settings_when_the_session_is_not_sandboxed() {
    let dir = tempfile::tempdir().unwrap();
    assert!(!build_args(&base_spec(dir.path())).contains(&"--settings".to_string()));
}

#[test]
fn build_args_turns_on_the_sandbox_as_the_very_last_flag_when_the_session_is_sandboxed() {
    let dir = tempfile::tempdir().unwrap();
    let mut spec = base_spec(dir.path());
    spec.sandbox = true;
    let args = build_args(&spec);
    assert_eq!(args[args.len() - 2], "--settings");
    let settings: Value = serde_json::from_str(&args[args.len() - 1]).unwrap();
    assert_eq!(settings["sandbox"]["enabled"], json!(true));
    assert_eq!(settings["sandbox"]["autoAllowBashIfSandboxed"], json!(true));
}

#[test]
fn build_args_starts_a_local_session_with_model_sonnet() {
    let dir = tempfile::tempdir().unwrap();
    let args = build_args(&local_spec(dir.path()));
    let idx = args.iter().position(|a| a == "--model").unwrap();
    assert_eq!(args[idx + 1], "sonnet");
}

// SessionReader::line / result_of

#[test]
fn reader_returns_the_structured_result_from_a_successful_run() {
    let mut r = SessionReader::new(false);
    let line = r#"{"type":"result","subtype":"success","is_error":false,"structured_output":{"status":"pr_opened"},"total_cost_usd":2.5,"session_id":"abc"}"#;
    let result = r.line(line, "known").unwrap();
    assert!(result.ok);
    assert_eq!(result.subtype, "success");
    assert_eq!(result.structured, Some(json!({"status": "pr_opened"})));
    assert_eq!(result.cost_usd, Some(2.5));
    assert_eq!(result.session_id, "abc");
    assert_eq!(result.error, None);
}

#[test]
fn reader_falls_back_to_the_known_session_id_when_the_event_has_none() {
    let mut r = SessionReader::new(false);
    let line = r#"{"type":"result","subtype":"success","is_error":false,"structured_output":{}}"#;
    let result = r.line(line, "known-id").unwrap();
    assert_eq!(result.session_id, "known-id");
}

#[test]
fn reader_reports_missing_structured_output_as_an_error() {
    let mut r = SessionReader::new(false);
    let line = r#"{"type":"result","subtype":"success","is_error":false,"structured_output":null,"session_id":"abc"}"#;
    let result = r.line(line, "known").unwrap();
    assert!(!result.ok);
    assert_eq!(result.error, Some("No structured output returned".to_string()));
}

#[test]
fn reader_joins_the_errors_array_when_the_run_failed() {
    let mut r = SessionReader::new(false);
    let line = r#"{"type":"result","subtype":"error_during_execution","is_error":true,"errors":["No conversation found with session ID: x"],"session_id":"abc"}"#;
    let result = r.line(line, "known").unwrap();
    assert!(!result.ok);
    assert_eq!(result.subtype, "error_during_execution");
    assert_eq!(
        result.error,
        Some("No conversation found with session ID: x".to_string())
    );
}

#[test]
fn reader_falls_back_to_the_result_string_when_there_is_no_errors_array() {
    let mut r = SessionReader::new(false);
    let line = r#"{"type":"result","subtype":"success","is_error":true,"result":"boom","session_id":"abc"}"#;
    let result = r.line(line, "known").unwrap();
    assert!(!result.ok);
    assert_eq!(result.subtype, "error");
    assert_eq!(result.error, Some("boom".to_string()));

    // No result string either: error text keeps the event's own subtype, while the field
    // itself is rewritten to "error".
    let line = r#"{"type":"result","subtype":"success","is_error":true,"session_id":"abc"}"#;
    let result = r.line(line, "known").unwrap();
    assert!(!result.ok);
    assert_eq!(result.subtype, "error");
    assert_eq!(result.error, Some("success".to_string()));
}

#[test]
fn reader_falls_back_to_the_subtype_when_a_failed_run_has_no_errors_or_result_string() {
    let mut r = SessionReader::new(false);
    let line = r#"{"type":"result","subtype":"error_during_execution","is_error":true,"session_id":"abc"}"#;
    let result = r.line(line, "known").unwrap();
    assert!(!result.ok);
    assert_eq!(result.subtype, "error_during_execution");
    assert_eq!(result.error, Some("error_during_execution".to_string()));
}

#[test]
fn reader_requires_subtype_success_for_ok_even_with_structured_output() {
    let mut r = SessionReader::new(false);
    let line = r#"{"type":"result","subtype":"error_max_turns","is_error":false,"structured_output":{"status":"pr_opened"},"session_id":"abc"}"#;
    let result = r.line(line, "known").unwrap();
    assert!(!result.ok);
    assert_eq!(result.subtype, "error_max_turns");
    assert_eq!(result.structured, Some(json!({"status": "pr_opened"})));
    assert_eq!(result.error, None);
}

#[test]
fn reader_ignores_non_result_events() {
    let mut r = SessionReader::new(false);
    assert!(r.line(r#"{"type":"system","subtype":"init"}"#, "known").is_none());
}

#[test]
fn reader_tracks_context_tokens_from_the_sessions_own_turns_only() {
    let mut r = SessionReader::new(false);
    r.line(
        r#"{"type":"assistant","parent_tool_use_id":"tool-1","message":{"usage":{"input_tokens":999999}}}"#,
        "known",
    );
    let before_any_turn = r
        .line(
            r#"{"type":"result","subtype":"success","is_error":false,"structured_output":{},"session_id":"abc"}"#,
            "known",
        )
        .unwrap();
    assert_eq!(before_any_turn.context_tokens, None);

    // Real stream-json main-thread turns carry null, not a missing field.
    r.line(
        r#"{"type":"assistant","parent_tool_use_id":null,"message":{"usage":{"input_tokens":100,"cache_read_input_tokens":20,"cache_creation_input_tokens":5}}}"#,
        "known",
    );
    let after_a_turn = r
        .line(
            r#"{"type":"result","subtype":"success","is_error":false,"structured_output":{},"session_id":"abc"}"#,
            "known",
        )
        .unwrap();
    assert_eq!(after_a_turn.context_tokens, Some(125));

    // A later turn with no usage (or usage without input_tokens) keeps the previous value.
    r.line(
        r#"{"type":"assistant","parent_tool_use_id":null,"message":{}}"#,
        "known",
    );
    r.line(
        r#"{"type":"assistant","parent_tool_use_id":null,"message":{"usage":{"cache_read_input_tokens":50}}}"#,
        "known",
    );
    let still_previous = r
        .line(
            r#"{"type":"result","subtype":"success","is_error":false,"structured_output":{},"session_id":"abc"}"#,
            "known",
        )
        .unwrap();
    assert_eq!(still_previous.context_tokens, Some(125));
}

#[test]
fn reader_keeps_the_latest_rate_limit_info_as_usage() {
    let mut r = SessionReader::new(false);
    r.line(
        r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","resetsAt":1000,"rateLimitType":"five_hour"}}"#,
        "known",
    );
    r.line(
        r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed_warning","resetsAt":2000,"rateLimitType":"five_hour"}}"#,
        "known",
    );
    let result = r
        .line(
            r#"{"type":"result","subtype":"success","is_error":false,"structured_output":{},"session_id":"abc"}"#,
            "known",
        )
        .unwrap();
    assert_eq!(
        result.usage,
        Some(json!({"status": "allowed_warning", "resetsAt": 2000, "rateLimitType": "five_hour"}))
    );
}

#[test]
fn reader_reports_no_usage_when_the_stream_never_sent_a_rate_limit_event() {
    let mut r = SessionReader::new(false);
    let result = r
        .line(
            r#"{"type":"result","subtype":"success","is_error":false,"structured_output":{},"session_id":"abc"}"#,
            "known",
        )
        .unwrap();
    assert_eq!(result.usage, None);
}

#[test]
fn reader_ignores_a_rate_limit_event_with_no_info() {
    let mut r = SessionReader::new(false);
    assert!(
        r.line(r#"{"type":"rate_limit_event","rate_limit_info":null}"#, "known")
            .is_none()
    );
    let result = r
        .line(
            r#"{"type":"result","subtype":"success","is_error":false,"structured_output":{},"session_id":"abc"}"#,
            "known",
        )
        .unwrap();
    assert_eq!(result.usage, None);
}

#[test]
fn no_result_still_carries_the_usage_seen_before_the_crash() {
    let mut r = SessionReader::new(false);
    r.line(
        r#"{"type":"rate_limit_event","rate_limit_info":{"status":"rejected"}}"#,
        "known",
    );
    let result = r.no_result(Some(1), "", "known");
    assert_eq!(result.usage, Some(json!({"status": "rejected"})));
}

// SessionReader::no_result / tail

#[test]
fn no_result_names_the_exit_code_and_tails_the_trimmed_stderr() {
    let r = SessionReader::new(false);
    let long_stderr = format!("  {}  ", "x".repeat(2000));
    let result = r.no_result(Some(1), &long_stderr, "known");
    assert!(!result.ok);
    assert_eq!(result.subtype, "exited");
    assert_eq!(result.session_id, "known");
    let err = result.error.unwrap();
    assert!(err.starts_with("claude exited with code 1 and no result."));
    // Prefix + space + 1500 of the trimmed body; no leading/trailing spaces from stderr.
    assert!(err.ends_with(&"x".repeat(1500)));
    assert!(!err.contains("  x"));
    assert!(err.len() < long_stderr.len());
}

#[test]
fn tail_keeps_only_the_last_n_characters() {
    assert_eq!(tail("hello world", 5), "world");
    assert_eq!(tail("hi", 10), "hi");
}

// signed_out / describe_failure (claude-signed-out.md section 3.1)

fn error_result_line(extra: &str) -> String {
    format!(r#"{{"type":"result","subtype":"success","is_error":true,{extra},"session_id":"s1"}}"#)
}

#[test]
fn reader_marks_a_run_out_sign_in_as_signed_out() {
    let mut r = SessionReader::new(false);
    let result = r
        .line(
            &error_result_line(
                r#""result":"Failed to authenticate: OAuth session expired and could not be refreshed""#,
            ),
            "s1",
        )
        .unwrap();
    assert!(result.signed_out);
    assert!(!result.limit_hit);

    let mut r = SessionReader::new(false);
    let result = r
        .line(
            r#"{"type":"result","subtype":"error_during_execution","is_error":true,"errors":["Not logged in · Please run /login"],"session_id":"s1"}"#,
            "s1",
        )
        .unwrap();
    assert!(result.signed_out);

    let mut r = SessionReader::new(false);
    let result = r
        .line(
            &error_result_line(
                r#""result":"API Error: 401 {\"type\":\"error\",\"error\":{\"type\":\"authentication_error\"}}""#,
            ),
            "s1",
        )
        .unwrap();
    assert!(result.signed_out);

    let result = SessionReader::new(false).no_result(Some(1), "Invalid API key · Please run /login", "s1");
    assert!(result.signed_out, "from stderr");
}

#[test]
fn reader_does_not_mark_a_limit_budget_ordinary_error_or_local_as_signed_out() {
    let mut r = SessionReader::new(false);
    let result = r
        .line(
            r#"{"type":"result","subtype":"success","is_error":true,"api_error_status":429,"result":"limit","session_id":"s1"}"#,
            "s1",
        )
        .unwrap();
    assert!(result.limit_hit);
    assert!(!result.signed_out);

    let mut r = SessionReader::new(false);
    let result = r
        .line(
            r#"{"type":"result","subtype":"error_max_budget_usd","is_error":true,"session_id":"s1"}"#,
            "s1",
        )
        .unwrap();
    assert!(!result.signed_out);

    let mut r = SessionReader::new(false);
    let result = r
        .line(
            r#"{"type":"result","subtype":"error_during_execution","is_error":true,"errors":["Tool failed"],"session_id":"s1"}"#,
            "s1",
        )
        .unwrap();
    assert!(!result.signed_out);

    let result = SessionReader::new(true).no_result(Some(1), "Not logged in", "s1");
    assert!(!result.signed_out, "a local server signs nothing in");
    assert!(!result.limit_hit);
}

#[test]
fn reader_marks_limit_hit_from_a_rejected_rate_limit_on_error() {
    let mut r = SessionReader::new(false);
    r.line(
        r#"{"type":"rate_limit_event","rate_limit_info":{"status":"rejected"}}"#,
        "s1",
    );
    let result = r
        .line(
            r#"{"type":"result","subtype":"success","is_error":true,"result":"rate limited","session_id":"s1"}"#,
            "s1",
        )
        .unwrap();
    assert!(result.limit_hit);
    assert!(!result.signed_out);

    let mut r = SessionReader::new(false);
    r.line(
        r#"{"type":"rate_limit_event","rate_limit_info":{"status":"rejected"}}"#,
        "s1",
    );
    let exited = r.no_result(Some(1), "", "s1");
    assert!(exited.limit_hit);
    assert!(!exited.signed_out);
}

#[test]
fn claude_engine_dispatches_args_env_prompt_and_reader_through_the_trait() {
    let dir = tempfile::tempdir().unwrap();
    let engine = ClaudeEngine;
    let spec = base_spec(dir.path());
    assert_eq!(engine.id(), EngineId::Claude);
    assert_eq!(engine.args(&spec), build_args(&spec));
    assert_eq!(engine.env(&spec), session_env(&spec));
    assert_eq!(engine.prompt(&spec), spec.prompt);
    let mut reader = engine.reader(&spec);
    let line = r#"{"type":"result","subtype":"success","is_error":false,"structured_output":{},"session_id":"abc"}"#;
    assert!(reader.line(line, "known").unwrap().ok);
}
