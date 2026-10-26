use super::*;

fn out(code: i32, stdout: &str) -> RunOutput {
    RunOutput {
        code: Some(code),
        stdout: stdout.to_string(),
        stderr: String::new(),
    }
}

fn out_err(code: i32, stdout: &str, stderr: &str) -> RunOutput {
    RunOutput {
        code: Some(code),
        stdout: stdout.to_string(),
        stderr: stderr.to_string(),
    }
}

// first_line

#[test]
fn first_line_takes_the_first_non_blank_line_trimmed() {
    assert_eq!(first_line("  git version 2.45.1  \nextra\n"), "git version 2.45.1");
    assert_eq!(first_line("\n\n  \nfoo"), "foo");
    assert_eq!(first_line(""), "");
}

#[test]
fn first_line_cuts_at_200_chars() {
    let long = "a".repeat(250);
    assert_eq!(first_line(&long).chars().count(), 200);
}

// git_check

#[test]
fn git_check_passes_with_the_version_line() {
    let r = out(0, "git version 2.45.1\n");
    assert_eq!(git_check(Some(&r)), SetupCheck::pass("git version 2.45.1"));
}

#[test]
fn git_check_fails_when_missing_or_non_zero() {
    assert_eq!(git_check(None), SetupCheck::fail("git isn't installed", fixes::GIT));
    assert_eq!(
        git_check(Some(&out(1, ""))),
        SetupCheck::fail("git isn't installed", fixes::GIT)
    );
}

// gh_installed_check / gh_check (its own check, apart from signed in)

#[test]
fn gh_installed_check_passes_with_the_version_line_and_fails_when_missing() {
    let r = out(0, "gh version 2.103.0 (2026-01-01)\n");
    assert_eq!(
        gh_installed_check(Some(&r)),
        SetupCheck::pass("gh version 2.103.0 (2026-01-01)")
    );
    assert_eq!(
        gh_installed_check(None),
        SetupCheck::fail("The GitHub CLI isn't installed", fixes::GH_MISSING)
    );
}

#[test]
fn gh_check_passes_on_exit_0_and_fails_signed_out_or_missing() {
    assert_eq!(gh_check(Some(&out(0, ""))), SetupCheck::pass("gh is signed in"));
    assert_eq!(
        gh_check(Some(&out(1, ""))),
        SetupCheck::fail("gh isn't signed in", fixes::GH)
    );
    assert_eq!(
        gh_check(None),
        SetupCheck::fail("The GitHub CLI isn't installed", fixes::GH_MISSING)
    );
}

// claude_check

#[test]
fn claude_check_reads_the_version_out_of_the_first_line() {
    let r = out(0, "Claude Code 2.1.4 (Claude Code)\n");
    assert_eq!(claude_check(Some(&r)), SetupCheck::pass("Claude Code 2.1.4"));
}

#[test]
fn claude_check_passes_without_a_version_number_and_fails_when_missing() {
    let r = out(0, "Claude Code\n");
    assert_eq!(claude_check(Some(&r)), SetupCheck::pass("Claude Code"));
    assert_eq!(
        claude_check(None),
        SetupCheck::fail("Claude Code isn't installed", fixes::CLAUDE)
    );
    assert_eq!(
        claude_check(Some(&out(1, ""))),
        SetupCheck::fail("Claude Code isn't installed", fixes::CLAUDE)
    );
}

// claude_signed_in_check

#[test]
fn claude_signed_in_check_passes_on_the_api_key_without_asking() {
    assert_eq!(
        claude_signed_in_check(false, None, true),
        SetupCheck::pass("Claude Code uses ANTHROPIC_API_KEY")
    );
}

#[test]
fn claude_signed_in_check_is_unknown_until_claude_is_installed() {
    assert_eq!(
        claude_signed_in_check(false, None, false),
        SetupCheck::unknown("Checked once Claude Code is installed", None)
    );
}

#[test]
fn claude_signed_in_check_is_unknown_with_a_fix_when_the_probe_never_answered() {
    assert_eq!(
        claude_signed_in_check(true, None, false),
        SetupCheck::unknown(
            "Couldn't tell whether Claude Code is signed in; the trial change will show it",
            Some(fixes::CLAUDE_SIGNED_IN)
        )
    );
}

#[test]
fn claude_signed_in_check_fails_on_logged_in_false_or_a_not_signed_in_message() {
    let json = out(0, r#"{"loggedIn":false}"#);
    assert_eq!(
        claude_signed_in_check(true, Some(&json), false),
        SetupCheck::fail("Claude Code isn't signed in", fixes::CLAUDE_SIGNED_IN)
    );
    let text = out_err(1, "", "Error: not logged in\n");
    assert_eq!(
        claude_signed_in_check(true, Some(&text), false),
        SetupCheck::fail("Claude Code isn't signed in", fixes::CLAUDE_SIGNED_IN)
    );
}

#[test]
fn claude_signed_in_check_passes_on_logged_in_true_or_a_plain_ok_answer() {
    let json = out(0, r#"{"loggedIn":true}"#);
    assert_eq!(
        claude_signed_in_check(true, Some(&json), false),
        SetupCheck::pass("Claude Code is signed in")
    );
    let plain = out(0, "Logged in as alice@example.com\n");
    assert_eq!(
        claude_signed_in_check(true, Some(&plain), false),
        SetupCheck::pass("Claude Code is signed in")
    );
}

#[test]
fn claude_signed_in_check_is_unknown_for_an_old_claude_code_that_does_not_know_the_command() {
    let r = out_err(1, "", "error: unknown command 'auth'\n");
    assert_eq!(
        claude_signed_in_check(true, Some(&r), false),
        SetupCheck::unknown(
            "Couldn't tell whether Claude Code is signed in; the trial change will show it",
            Some(fixes::CLAUDE_SIGNED_IN)
        )
    );
}

#[test]
fn claude_signed_in_check_never_sees_the_api_key_or_email_in_its_detail() {
    let json = out(0, r#"{"loggedIn":true,"email":"alice@example.com"}"#);
    let check = claude_signed_in_check(true, Some(&json), false);
    assert!(!check.detail.contains("alice@example.com"));
}

// cursor_check

#[test]
fn cursor_check_fails_not_installed_when_the_probe_never_ran() {
    assert_eq!(
        cursor_check(false, None),
        SetupCheck::fail("Cursor's agent isn't installed", fixes::CURSOR_MISSING)
    );
}

#[test]
fn cursor_check_fails_not_signed_in_when_installed_but_status_fails() {
    assert_eq!(
        cursor_check(true, None),
        SetupCheck::fail("Cursor's agent isn't signed in", fixes::CURSOR)
    );
    assert_eq!(
        cursor_check(true, Some(&out(1, ""))),
        SetupCheck::fail("Cursor's agent isn't signed in", fixes::CURSOR)
    );
}

#[test]
fn cursor_check_passes_with_the_status_line_and_strips_its_own_checkmark() {
    let r = out(0, "✓ Logged in as a@b.c\n");
    assert_eq!(
        cursor_check(true, Some(&r)),
        SetupCheck::pass("Cursor's agent: Logged in as a@b.c")
    );
}

// resolve_cursor / resolve_cursor_on_path

#[test]
fn resolve_cursor_prefers_the_env_override_on_any_platform() {
    let env = |k: &str| (k == "RETASKABLE_CURSOR").then(|| "C:/custom/cursor-agent.exe".to_string());
    assert_eq!(
        resolve_cursor(env, true, &[], None),
        PathBuf::from("C:/custom/cursor-agent.exe")
    );
    assert_eq!(
        resolve_cursor(env, false, &[], None),
        PathBuf::from("C:/custom/cursor-agent.exe")
    );
}

#[test]
fn resolve_cursor_finds_cursor_agent_directly_on_the_path() {
    let dir = tempfile::tempdir().unwrap();
    let name = if cfg!(windows) {
        "cursor-agent.exe"
    } else {
        "cursor-agent"
    };
    std::fs::write(dir.path().join(name), "").unwrap();
    let no_env = |_: &str| None;
    let got = resolve_cursor(no_env, cfg!(windows), &[dir.path().to_path_buf()], None);
    assert_eq!(got, dir.path().join(name));
}

#[test]
fn resolve_cursor_falls_back_to_the_cmd_shim_on_windows() {
    if !cfg!(windows) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("cursor-agent.cmd"), "").unwrap();
    let no_env = |_: &str| None;
    let got = resolve_cursor(no_env, true, &[dir.path().to_path_buf()], None);
    assert_eq!(got, dir.path().join("cursor-agent.cmd"));
}

#[test]
fn resolve_cursor_falls_back_to_home_local_bin() {
    let home = tempfile::tempdir().unwrap();
    let name = if cfg!(windows) {
        "cursor-agent.exe"
    } else {
        "cursor-agent"
    };
    let bin_dir = home.path().join(".local").join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::fs::write(bin_dir.join(name), "").unwrap();
    let no_env = |_: &str| None;
    let got = resolve_cursor(no_env, cfg!(windows), &[], Some(home.path().to_path_buf()));
    assert_eq!(got, bin_dir.join(name));
}

#[test]
fn resolve_cursor_falls_back_to_the_bare_name_when_nothing_matches() {
    let no_env = |_: &str| None;
    let name = if cfg!(windows) {
        "cursor-agent.exe"
    } else {
        "cursor-agent"
    };
    assert_eq!(resolve_cursor(no_env, cfg!(windows), &[], None), PathBuf::from(name));
}

#[test]
fn resolve_cursor_on_path_reads_dirs_from_the_path_env() {
    let dir = tempfile::tempdir().unwrap();
    let name = if cfg!(windows) {
        "cursor-agent.exe"
    } else {
        "cursor-agent"
    };
    std::fs::write(dir.path().join(name), "").unwrap();
    let path_value = dir.path().to_string_lossy().into_owned();
    let env = move |k: &str| (k == "PATH").then(|| path_value.clone());
    assert_eq!(resolve_cursor_on_path(env), dir.path().join(name));
}

// claude_ready / engine_ready / show_cursor / setup_checks_pass

fn checks(claude: SetupCheck, claude_signed_in: SetupCheck, cursor: SetupCheck) -> SetupChecks {
    SetupChecks {
        git: SetupCheck::pass("git"),
        gh_installed: SetupCheck::pass("gh"),
        gh: SetupCheck::pass("gh signed in"),
        claude,
        claude_signed_in,
        cursor,
    }
}

#[test]
fn a_ready_machine_passes_with_the_documented_lines() {
    let c = checks(
        SetupCheck::pass("Claude Code 2.1.4"),
        SetupCheck::pass("Claude Code is signed in"),
        SetupCheck::fail("Cursor's agent isn't installed", fixes::CURSOR_MISSING),
    );
    assert!(claude_ready(&c));
    assert!(engine_ready(&c));
    assert!(!show_cursor(&c));
    assert!(setup_checks_pass(&c));
    assert_eq!(
        setup_check_lines(&c),
        vec![
            "✓ git",
            "✓ gh signed in",
            "✓ Claude Code 2.1.4",
            "✓ Claude Code is signed in"
        ]
        .into_iter()
        .map(String::from)
        .collect::<Vec<_>>()
    );
}

#[test]
fn couldnt_tell_whether_claude_is_signed_in_still_counts_as_ready() {
    let c = checks(
        SetupCheck::pass("Claude Code 2.1.4"),
        SetupCheck::unknown(
            "Couldn't tell whether Claude Code is signed in; the trial change will show it",
            Some(fixes::CLAUDE_SIGNED_IN),
        ),
        SetupCheck::fail("Cursor's agent isn't installed", fixes::CURSOR_MISSING),
    );
    assert!(claude_ready(&c));
    assert!(setup_checks_pass(&c));
}

#[test]
fn a_signed_in_cursor_is_enough_when_claude_isnt() {
    let c = checks(
        SetupCheck::fail("Claude Code isn't installed", fixes::CLAUDE),
        SetupCheck::unknown("Checked once Claude Code is installed", None),
        SetupCheck::pass("Cursor's agent: Logged in as a@b.c"),
    );
    assert!(!claude_ready(&c));
    assert!(engine_ready(&c));
    assert!(show_cursor(&c));
    assert!(setup_checks_pass(&c));
    assert!(setup_check_lines(&c).contains(&"✓ Cursor's agent: Logged in as a@b.c".to_string()));
}

#[test]
fn neither_engine_ready_fails_the_whole_check() {
    let c = checks(
        SetupCheck::fail("Claude Code isn't installed", fixes::CLAUDE),
        SetupCheck::unknown("Checked once Claude Code is installed", None),
        SetupCheck::fail("Cursor's agent isn't installed", fixes::CURSOR_MISSING),
    );
    assert!(!engine_ready(&c));
    assert!(!setup_checks_pass(&c));
}

// setup_check_line / setup_check_report

#[test]
fn setup_check_line_formats_each_state() {
    assert_eq!(
        setup_check_line(&SetupCheck::pass("git version 2.45.1")),
        "✓ git version 2.45.1"
    );
    assert_eq!(
        setup_check_line(&SetupCheck::fail(
            "git isn't installed",
            "Install git from git-scm.com."
        )),
        "✗ git isn't installed: install git from git-scm.com."
    );
    assert_eq!(
        setup_check_line(&SetupCheck::unknown("Checked once Claude Code is installed", None)),
        "? Checked once Claude Code is installed"
    );
}

#[test]
fn setup_check_report_has_the_setup_check_header_and_indented_lines() {
    let c = checks(
        SetupCheck::pass("Claude Code 2.1.4"),
        SetupCheck::pass("Claude Code is signed in"),
        SetupCheck::fail("Cursor's agent isn't installed", fixes::CURSOR_MISSING),
    );
    let report = setup_check_report(&c);
    assert!(report.starts_with("Setup check:\n"));
    assert!(report.contains("  ✓ Claude Code 2.1.4"));
}

// run_setup_checks: a smoke test of the wiring (git/gh/cursor are whatever's on this machine, so
// only structure and overall timeliness are checked).

#[tokio::test]
async fn run_setup_checks_completes_promptly_and_builds_every_field() {
    let no_env = |_: &str| None;
    let result = tokio::time::timeout(Duration::from_secs(25), run_setup_checks(no_env)).await;
    let c = result.expect("run_setup_checks should finish within its own 20s check budget");
    for check in [
        &c.git,
        &c.gh_installed,
        &c.gh,
        &c.claude,
        &c.claude_signed_in,
        &c.cursor,
    ] {
        assert!(!check.detail.is_empty());
    }
}

// run_timeout: the subprocess plumbing itself, with OS-native commands standing in so these
// tests don't need git/gh/claude installed (mirrors session/tests.rs's print_file_command).

#[tokio::test]
async fn run_timeout_captures_stdout_and_exit_code() {
    let dir = tempfile::tempdir().unwrap();
    let out_file = dir.path().join("out.txt");
    std::fs::write(&out_file, "hello\n").unwrap();
    #[cfg(windows)]
    let (bin, args): (PathBuf, Vec<String>) = (
        PathBuf::from("cmd"),
        vec![
            "/C".to_string(),
            "type".to_string(),
            out_file.to_string_lossy().into_owned(),
        ],
    );
    #[cfg(not(windows))]
    let (bin, args): (PathBuf, Vec<String>) = (PathBuf::from("cat"), vec![out_file.to_string_lossy().into_owned()]);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = run_timeout(&bin, &args, CHECK_TIMEOUT).await.unwrap();
    assert_eq!(result.code, Some(0));
    assert_eq!(first_line(&result.stdout), "hello");
}

#[tokio::test]
async fn run_timeout_returns_none_for_a_missing_binary() {
    let result = run_timeout("retaskable-no-such-binary", &["--version"], CHECK_TIMEOUT).await;
    assert!(result.is_none());
}

#[tokio::test]
async fn run_timeout_kills_a_hung_process_once_the_budget_runs_out() {
    #[cfg(windows)]
    let (bin, args) = (
        PathBuf::from("cmd"),
        vec![
            "/C".to_string(),
            "ping".to_string(),
            "-n".to_string(),
            "20".to_string(),
            "127.0.0.1".to_string(),
        ],
    );
    #[cfg(not(windows))]
    let (bin, args) = (PathBuf::from("sleep"), vec!["20".to_string()]);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let started = std::time::Instant::now();
    let result = run_timeout(&bin, &args, Duration::from_millis(100)).await;
    assert!(result.is_none());
    assert!(started.elapsed() < Duration::from_secs(5));
}
