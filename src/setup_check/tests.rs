use std::path::PathBuf;

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

// claude_ready / engine_ready / show_cursor / setup_checks_pass

fn checks(claude: SetupCheck, claude_signed_in: SetupCheck, cursor: SetupCheck) -> SetupChecks {
    SetupChecks {
        git: SetupCheck::pass("git"),
        gh_installed: SetupCheck::pass("gh"),
        gh: SetupCheck::pass("gh signed in"),
        claude,
        claude_signed_in,
        cursor,
        pi: None,
        codex: None,
        docker: None,
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
        setup_check_lines(&c, false),
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
    assert!(setup_check_lines(&c, false).contains(&"✓ Cursor's agent: Logged in as a@b.c".to_string()));
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
    let report = setup_check_report(&c, false);
    assert!(report.starts_with("Setup check:\n"));
    assert!(report.contains("  ✓ Claude Code 2.1.4"));
}

// run_setup_checks: a smoke test of the wiring (git/gh/cursor are whatever's on this machine, so
// only structure and overall timeliness are checked).

#[tokio::test]
async fn run_setup_checks_completes_promptly_and_builds_every_field() {
    let result = tokio::time::timeout(Duration::from_secs(25), run_setup_checks(&Env::empty(), true)).await;
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

// pi_check / codex_check

#[test]
fn pi_check_has_three_states() {
    assert_eq!(pi_check(None), SetupCheck::fail("pi isn't installed", fixes::PI));
    let long = "x".repeat(300);
    let failed = pi_check(Some(&out_err(1, "", &format!("\n{long}\nsecond"))));
    assert_eq!(failed, SetupCheck::fail("x".repeat(200), fixes::PI));
    assert_eq!(pi_check(Some(&out(0, "pi 1.2.3\n"))), SetupCheck::pass("pi 1.2.3"));
    assert_eq!(pi_check(Some(&out(0, "\n"))), SetupCheck::pass("pi"));
}

#[test]
fn codex_check_has_four_states() {
    let signed_in = out(0, "Logged in\n");
    assert_eq!(
        codex_check(None, None),
        SetupCheck::fail("Codex isn't installed", fixes::CODEX)
    );
    assert_eq!(
        codex_check(Some(&out_err(2, "", "bad flag\nmore")), None),
        SetupCheck::fail("bad flag", fixes::CODEX)
    );
    let long = "y".repeat(300);
    assert_eq!(
        codex_check(Some(&out_err(1, "", &long)), None),
        SetupCheck::fail("y".repeat(200), fixes::CODEX)
    );
    let version = out(0, "codex-cli 0.46.0\n");
    assert_eq!(
        codex_check(Some(&version), Some(&signed_in)),
        SetupCheck::pass("codex-cli 0.46.0")
    );
    let signed_out = codex_check(Some(&version), Some(&out(1, "")));
    assert_eq!(
        signed_out,
        SetupCheck::fail("Codex isn't signed in", fixes::CODEX_SIGNED_IN)
    );
    assert_eq!(codex_check(Some(&version), None), signed_out);
}

#[test]
fn a_signed_out_codex_leaves_the_checks_passing() {
    let mut c = checks(
        SetupCheck::pass("Claude Code 2.1.0"),
        SetupCheck::pass("Claude Code is signed in"),
        SetupCheck::fail("Cursor's agent isn't installed", fixes::CURSOR_MISSING),
    );
    c.codex = Some(SetupCheck::fail("Codex isn't signed in", fixes::CODEX_SIGNED_IN));
    c.pi = Some(SetupCheck::fail("pi isn't installed", fixes::PI));
    assert!(setup_checks_pass(&c));
    assert!(engine_ready(&c));
    assert!(claude_ready(&c));
}

#[test]
fn the_flag_off_keeps_the_old_pi_and_codex_result() {
    assert_eq!(plain_engine_check("pi", None), None);
    assert_eq!(plain_engine_check("pi", Some(&out(1, "oops"))), None);
    assert_eq!(
        plain_engine_check("Codex", Some(&out(0, "codex-cli 0.5.0\n"))),
        Some(SetupCheck::pass("codex-cli 0.5.0"))
    );
    assert_eq!(
        plain_engine_check("pi", Some(&out(0, "\n"))),
        Some(SetupCheck::pass("pi"))
    );
}

#[test]
fn pi_and_codex_lines_follow_the_probe_flag_and_the_found_binaries() {
    let mut c = checks(
        SetupCheck::pass("Claude Code 2.1.0"),
        SetupCheck::pass("Claude Code is signed in"),
        SetupCheck::fail("Cursor's agent isn't installed", fixes::CURSOR_MISSING),
    );
    let today = setup_check_lines(&c, true);
    assert_eq!(today, setup_check_lines(&c, false));
    c.pi = Some(SetupCheck::pass("pi 0.3.1"));
    c.codex = Some(SetupCheck::fail("Codex isn't signed in", fixes::CODEX_SIGNED_IN));
    let report = setup_check_report(&c, true);
    assert!(report.contains("  ✓ pi: pi 0.3.1"));
    assert!(report.contains("  ✗ Codex isn't signed in: run `codex login`."));
    assert!(setup_checks_pass(&c));
    assert_eq!(setup_check_lines(&c, false), today);
    c.codex = Some(SetupCheck::pass("codex-cli 0.46.0"));
    assert!(setup_check_lines(&c, true).contains(&"✓ Codex: codex-cli 0.46.0".to_string()));
}

// docker line (agent-ci.md section 3.6)

#[test]
fn the_docker_line_shows_only_when_there_is_a_docker_check() {
    let mut c = checks(
        SetupCheck::pass("Claude Code 2.1.0"),
        SetupCheck::pass("Claude Code is signed in"),
        SetupCheck::pass("Cursor"),
    );
    assert!(!setup_check_report(&c, false).contains("Docker"));
    c.docker = Some(SetupCheck::fail(
        "Docker isn't running: this computer won't run CI",
        fixes::DOCKER,
    ));
    let lines = setup_check_lines(&c, false);
    assert_eq!(
        lines.last().unwrap(),
        "✗ Docker isn't running: this computer won't run CI: start Docker (or install it) on this computer."
    );
}

#[tokio::test]
async fn docker_check_reads_the_reason_from_the_probe() {
    let c = docker_check("retaskable-no-such-docker", false, None).await;
    assert_eq!(c.ok, Some(false));
    assert!(c.detail.contains("isn't installed"));
    let c = docker_check("docker", true, Some("img")).await;
    assert!(c.detail.contains("own Docker image"));
}

// SetupCheckRunner and to_json

mod runner_tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use serde_json::json;
    use tokio::sync::{Mutex, Notify};

    use super::super::*;
    use super::checks;

    fn with_git(detail: &str) -> SetupChecks {
        let mut c = checks(SetupCheck::pass("a"), SetupCheck::pass("b"), SetupCheck::pass("c"));
        c.git = SetupCheck::pass(detail);
        c
    }

    /// A probe that counts its runs and answers with the next of `answers` (the last repeats).
    fn stub(answers: Vec<SetupChecks>) -> (Arc<AtomicUsize>, Arc<Notify>, runner::Probe) {
        let runs = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(Notify::new());
        let (r, g) = (runs.clone(), gate.clone());
        let probe: runner::Probe = Box::new(move || {
            let n = r.fetch_add(1, Ordering::SeqCst);
            let answer = answers[n.min(answers.len() - 1)].clone();
            let g = g.clone();
            Box::pin(async move {
                g.notified().await;
                answer
            })
        });
        (runs, gate, probe)
    }

    #[tokio::test]
    async fn latest_is_none_before_the_first_run() {
        let (_, _, probe) = stub(vec![with_git("1")]);
        assert_eq!(SetupCheckRunner::with_probe(probe).latest(), None);
    }

    #[tokio::test]
    async fn two_run_nows_at_once_run_the_probe_once() {
        let (runs, gate, probe) = stub(vec![with_git("1"), with_git("2")]);
        let runner = Arc::new(SetupCheckRunner::with_probe(probe));
        let (a, b) = (runner.clone(), runner.clone());
        let (a, b) = (
            tokio::spawn(async move { a.run_now().await }),
            tokio::spawn(async move { b.run_now().await }),
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        gate.notify_one();
        assert_eq!(a.await.unwrap(), with_git("1"));
        assert_eq!(b.await.unwrap(), with_git("1"));
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert_eq!(runner.latest(), Some(with_git("1")));
    }

    #[tokio::test]
    async fn subscribe_ticks_on_first_and_changed_results_only() {
        let (_, gate, probe) = stub(vec![with_git("1"), with_git("1"), with_git("2")]);
        let runner = SetupCheckRunner::with_probe(probe);
        let mut rx = runner.subscribe();
        gate.notify_one();
        runner.run_now().await;
        assert!(rx.has_changed().unwrap());
        assert_eq!(*rx.borrow_and_update(), Some(with_git("1")));
        gate.notify_one();
        runner.run_now().await;
        assert!(!rx.has_changed().unwrap());
        gate.notify_one();
        runner.run_now().await;
        assert!(rx.has_changed().unwrap());
        assert_eq!(*rx.borrow_and_update(), Some(with_git("2")));
    }

    #[tokio::test]
    async fn reruns_after_600_seconds_of_the_fake_clock_and_not_before() {
        let (runs, gate, probe) = stub(vec![with_git("1")]);
        let tick = Arc::new(Notify::new());
        let asked = Arc::new(Mutex::new(Vec::new()));
        let (t, a) = (tick.clone(), asked.clone());
        let sleep: runner::Sleep = Box::new(move |d| {
            let (t, a) = (t.clone(), a.clone());
            Box::pin(async move {
                a.lock().await.push(d);
                t.notified().await;
            })
        });
        let runner = Arc::new(SetupCheckRunner::new(probe, sleep));
        let reported = Arc::new(AtomicUsize::new(0));
        let r = reported.clone();
        runner.spawn(move |_| {
            Box::pin(async move {
                r.fetch_add(1, Ordering::SeqCst);
            })
        });
        gate.notify_one();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!((runs.load(Ordering::SeqCst), reported.load(Ordering::SeqCst)), (1, 1));
        assert_eq!(*asked.lock().await, vec![Duration::from_secs(600)]);
        // The clock hasn't moved: no second run.
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        tick.notify_one();
        gate.notify_one();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(runs.load(Ordering::SeqCst), 2);
        assert_eq!(reported.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn to_json_has_the_camel_case_shape() {
        let mut c = checks(
            SetupCheck::pass("claude ok"),
            SetupCheck::unknown("can't tell", None),
            SetupCheck::fail("no cursor", "Install it."),
        );
        let v = c.to_json();
        assert_eq!(v["git"], json!({ "ok": true, "detail": "git" }));
        assert_eq!(v["claudeSignedIn"], json!({ "ok": null, "detail": "can't tell" }));
        assert!(v["claudeSignedIn"].get("fix").is_none());
        assert_eq!(
            v["cursor"],
            json!({ "ok": false, "detail": "no cursor", "fix": "Install it." })
        );
        for k in ["ghInstalled", "gh", "claude"] {
            assert!(v.get(k).is_some(), "{k}");
        }
        assert_eq!(
            v["node"],
            json!({ "ok": true, "detail": "Not needed: this agent is a compiled program" })
        );
        for k in ["pi", "codex", "docker"] {
            assert!(v.get(k).is_none(), "{k}");
        }
        c.pi = Some(SetupCheck::pass("pi 1"));
        c.docker = Some(SetupCheck::pass("d"));
        let v = c.to_json();
        assert_eq!(v["pi"], json!({ "ok": true, "detail": "pi 1" }));
        assert!(v.get("docker").is_some() && v.get("codex").is_none());
    }
}
