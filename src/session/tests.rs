use super::*;
use serde_json::json;

fn base_spec(cwd: &Path) -> SessionSpec {
    SessionSpec {
        model: "sonnet".to_string(),
        prompt: "do the thing".to_string(),
        schema: json!({"type": "object"}),
        cwd: cwd.to_path_buf(),
        max_turns: None,
        budget_usd: 0.0,
        start: SessionStart::Ephemeral,
    }
}

// resolve_claude / resolve_claude_on_path

#[test]
fn resolve_claude_prefers_the_env_override_on_any_platform() {
    let env = |k: &str| (k == "RETASKABLE_CLAUDE").then(|| "C:/custom/claude.exe".to_string());
    assert_eq!(
        resolve_claude(env, true, &[]).unwrap(),
        PathBuf::from("C:/custom/claude.exe")
    );
    assert_eq!(
        resolve_claude(env, false, &[]).unwrap(),
        PathBuf::from("C:/custom/claude.exe")
    );
}

#[test]
fn resolve_claude_is_plain_claude_off_windows() {
    let no_env = |_: &str| None;
    assert_eq!(resolve_claude(no_env, false, &[]).unwrap(), PathBuf::from("claude"));
}

#[test]
fn resolve_claude_finds_claude_exe_directly_on_the_path_on_windows() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("claude.exe"), "").unwrap();
    let no_env = |_: &str| None;
    let got = resolve_claude(no_env, true, &[dir.path().to_path_buf()]).unwrap();
    assert_eq!(got, dir.path().join("claude.exe"));
}

#[test]
fn resolve_claude_goes_behind_the_npm_shim_on_windows() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("claude.cmd"), "").unwrap();
    let behind = dir
        .path()
        .join("node_modules")
        .join("@anthropic-ai")
        .join("claude-code")
        .join("bin");
    std::fs::create_dir_all(&behind).unwrap();
    std::fs::write(behind.join("claude.exe"), "").unwrap();
    let no_env = |_: &str| None;
    let got = resolve_claude(no_env, true, &[dir.path().to_path_buf()]).unwrap();
    assert_eq!(got, behind.join("claude.exe"));
}

#[test]
fn resolve_claude_fails_naming_the_fix_when_nothing_matches_on_windows() {
    let no_env = |_: &str| None;
    let err = resolve_claude(no_env, true, &[]).unwrap_err();
    assert!(err.to_string().contains("RETASKABLE_CLAUDE"));
}

#[test]
fn resolve_claude_on_path_reads_dirs_from_the_path_env_on_windows() {
    if !cfg!(windows) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("claude.exe"), "").unwrap();
    let path_value = dir.path().to_string_lossy().into_owned();
    let env = move |k: &str| (k == "PATH").then(|| path_value.clone());
    assert_eq!(resolve_claude_on_path(env).unwrap(), dir.path().join("claude.exe"));
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

#[test]
fn session_env_runs_subagents_on_sonnet_by_default_and_haiku_for_a_haiku_session() {
    assert!(session_env("sonnet").contains(&("CLAUDE_CODE_SUBAGENT_MODEL".to_string(), "sonnet".to_string())));
    assert!(session_env("opus").contains(&("CLAUDE_CODE_SUBAGENT_MODEL".to_string(), "sonnet".to_string())));
    assert!(session_env("haiku").contains(&("CLAUDE_CODE_SUBAGENT_MODEL".to_string(), "haiku".to_string())));
}

#[test]
fn session_env_disables_claude_ai_connectors_and_sets_the_compact_window() {
    let env = session_env("sonnet");
    assert!(env.contains(&("ENABLE_CLAUDEAI_MCP_SERVERS".to_string(), "false".to_string())));
    assert!(env.contains(&("CLAUDE_CODE_AUTO_COMPACT_WINDOW".to_string(), "200000".to_string())));
}

#[test]
fn session_env_overwrites_the_cloudflare_token_so_nothing_can_deploy() {
    let env = session_env("sonnet");
    assert!(
        env.iter()
            .any(|(k, v)| k == "CLOUDFLARE_API_TOKEN" && v == NO_DEPLOY_TOKEN)
    );
    assert!(env.iter().any(|(k, v)| k == "CF_API_TOKEN" && v == NO_DEPLOY_TOKEN));
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
    assert_eq!(result.cost_usd, 2.5);
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

// spawn_and_read: the engine-neutral plumbing, exercised with OS-native commands standing in
// for `claude` so these tests don't need the real binary installed.

#[cfg(windows)]
fn print_file_command(file: &Path) -> (PathBuf, Vec<String>) {
    (
        PathBuf::from("cmd"),
        vec![
            "/C".to_string(),
            "type".to_string(),
            file.to_string_lossy().into_owned(),
        ],
    )
}

#[cfg(not(windows))]
fn print_file_command(file: &Path) -> (PathBuf, Vec<String>) {
    (PathBuf::from("cat"), vec![file.to_string_lossy().into_owned()])
}

#[tokio::test]
async fn spawn_and_read_returns_the_result_event_a_session_printed() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.jsonl");
    std::fs::write(
        &out,
        r#"{"type":"result","subtype":"success","is_error":false,"structured_output":{"status":"pr_opened"},"total_cost_usd":1.25,"session_id":"sess-1"}"#,
    )
    .unwrap();
    let (bin, args) = print_file_command(&out);
    let result = spawn_and_read(&bin, &args, &[], dir.path(), "hi", "known", false)
        .await
        .unwrap();
    assert!(result.ok);
    assert_eq!(result.subtype, "success");
    assert_eq!(result.structured, Some(json!({"status": "pr_opened"})));
    assert_eq!(result.cost_usd, 1.25);
    assert_eq!(result.session_id, "sess-1");
    assert!(!result.signed_out);
    assert!(!result.limit_hit);
}

#[tokio::test]
async fn spawn_and_read_reports_no_result_when_the_session_exits_without_one() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("empty.jsonl");
    std::fs::write(&out, "").unwrap();
    let (bin, args) = print_file_command(&out);
    let result = spawn_and_read(&bin, &args, &[], dir.path(), "hi", "known", false)
        .await
        .unwrap();
    assert!(!result.ok);
    assert_eq!(result.subtype, "exited");
    assert_eq!(result.session_id, "known");
    assert!(result.error.unwrap().contains("exited with code 0"));
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
fn signed_out_is_only_a_failed_claude_result_never_cursor() {
    assert!(is_signed_out(false, Some("Failed to authenticate"), Some("claude")));
    assert!(!is_signed_out(false, Some("Failed to authenticate"), Some("cursor")));
    assert!(!is_signed_out(true, Some("Failed to authenticate"), None));
    assert!(signed_out_text(Some(
        "Implementer ended with error: Failed to authenticate: OAuth session expired"
    )));
}

#[test]
fn describe_failure_names_signed_out_limit_budget_and_ordinary_errors() {
    let signed = SessionResult {
        ok: false,
        subtype: "error".to_string(),
        structured: None,
        error: Some("Not logged in".to_string()),
        cost_usd: 0.0,
        session_id: "s".to_string(),
        limit_hit: false,
        signed_out: true,
        context_tokens: None,
        usage: None,
    };
    assert_eq!(
        describe_failure(&signed, 5.0),
        "found Claude signed out on this computer (Not logged in)"
    );

    let limit = SessionResult {
        signed_out: false,
        limit_hit: true,
        error: Some("rate limited".to_string()),
        ..signed.clone()
    };
    assert_eq!(describe_failure(&limit, 5.0), "hit the usage limit");

    let budget = SessionResult {
        subtype: "error_max_budget_usd".to_string(),
        cost_usd: 4.2,
        limit_hit: false,
        signed_out: false,
        error: None,
        ..signed.clone()
    };
    assert_eq!(describe_failure(&budget, 5.0), "hit its $5 budget (spent $4.20)");

    let ordinary = SessionResult {
        subtype: "error_during_execution".to_string(),
        error: Some("tool crashed".to_string()),
        limit_hit: false,
        signed_out: false,
        ..signed
    };
    assert_eq!(
        describe_failure(&ordinary, 5.0),
        "ended with error_during_execution: tool crashed"
    );
}
