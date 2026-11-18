use super::*;
use crate::env::Env;
use std::sync::atomic::{AtomicUsize, Ordering};

type Hook = Arc<Mutex<Option<Box<dyn Fn() + Send>>>>;
type Calls = Arc<Mutex<Vec<(String, Vec<String>)>>>;

struct Rig {
    setup: Arc<Setup>,
    calls: Calls,
    data: tempfile::TempDir,
    refreshed: Arc<AtomicUsize>,
    /// Runs when a program does, to change what the checks will say.
    hook: Hook,
}

fn checks_with(git: bool, gh: bool, gh_in: bool, claude: bool, signed: bool) -> SetupChecks {
    let mut c = SetupChecks::all_unknown();
    let ck = |ok: bool| crate::setup_check::SetupCheck {
        ok: Some(ok),
        detail: String::new(),
        fix: None,
        info: false,
    };
    c.git = ck(git);
    c.gh = ck(gh);
    c.gh_installed = ck(gh_in);
    c.claude = ck(claude);
    c.claude_signed_in = ck(signed);
    c
}

/// A rig on `os`, whose checks start as `checks` and whose programs answer `code`.
fn rig(os: &str, checks: SetupChecks, code: i32, stderr: &str) -> Rig {
    let runner = Arc::new(SetupCheckRunner::with_probe(Box::new(move || {
        let c = checks.clone();
        Box::pin(async move { c })
    })));
    let calls: Calls = Arc::default();
    let refreshed = Arc::new(AtomicUsize::new(0));
    let data = tempfile::tempdir().unwrap();
    let hook: Hook = Arc::default();
    let h = hook.clone();
    let c = calls.clone();
    let stderr = stderr.to_string();
    let r = refreshed.clone();
    let latest = runner.clone();
    let deps = Deps {
        run: Arc::new(move |p, a, _| {
            c.lock().unwrap().push((p.to_string(), a.to_vec()));
            if let Some(f) = h.lock().unwrap().as_ref() {
                f();
            }
            Ran {
                code: Some(code),
                stderr: stderr.clone(),
                ..Ran::default()
            }
        }),
        download: Arc::new(|url| Ok(format!("downloaded {url}").into_bytes())),
        extract: Arc::new(|_, _| Ok(())),
        open: Arc::new(|_| {}),
        ask: Arc::new(|| None),
        wait: Arc::new(|_| {}),
        refresh: Arc::new(move || {
            r.fetch_add(1, Ordering::SeqCst);
            latest.latest()
        }),
        use_gh: Arc::new(|_| {}),
        use_git: Arc::new(|_| {}),
        use_local_bin: Arc::new(|_| {}),
        log: Arc::new(|_| {}),
    };
    let setup = Setup::with(
        runner,
        Default::default(),
        data.path().to_path_buf(),
        PathBuf::from("/home/x"),
        os.into(),
        "x86_64".into(),
        deps,
    );
    Rig {
        setup,
        calls,
        data,
        refreshed,
        hook,
    }
}

fn seed(rig: &Rig, checks: SetupChecks) {
    *rig.setup.checks.shared().lock().unwrap() = Some(checks);
}

fn missing() -> SetupChecks {
    checks_with(false, false, false, false, false)
}

#[test]
fn an_installed_tool_is_refused() {
    let r = rig("linux", missing(), 0, "");
    seed(&r, checks_with(true, true, true, true, true));
    assert_eq!(r.setup.install_refusal("gh").as_deref(), Some("Already installed."));
    assert_eq!(r.setup.install_refusal("claude").as_deref(), Some("Already installed."));
}

#[test]
fn git_on_linux_is_the_persons_to_install_and_another_os_isnt_ours_to() {
    let r = rig("linux", missing(), 0, "");
    seed(&r, missing());
    assert_eq!(
        r.setup.install_refusal("git").as_deref(),
        Some("Installing git needs sudo: copy the command instead.")
    );
    let r = rig("freebsd", missing(), 0, "");
    seed(&r, missing());
    assert_eq!(
        r.setup.install_refusal("git").as_deref(),
        Some("Retaskable can't install git on this computer.")
    );
    assert_eq!(r.setup.install_refusal("gh"), None);
}

#[test]
fn claude_code_runs_anthropics_installer_with_an_argument_list_and_deletes_it() {
    let r = rig("linux", missing(), 0, "");
    seed(&r, missing());
    r.setup.install("claude").unwrap();
    let calls = r.calls.lock().unwrap().clone();
    let file = r.data.path().join("downloads").join("claude-install.sh");
    assert_eq!(
        calls,
        vec![("/bin/bash".to_string(), vec![file.to_string_lossy().into_owned()])]
    );
    assert!(!file.exists());
    let a = r.setup.activity();
    assert_eq!(a["last"]["ok"], true);
    assert_eq!(a["last"]["detail"], "Claude Code is installed.");
    assert_eq!(a["last"]["action"], "install");
    assert!(a.get("running").is_none());
    assert!(r.refreshed.load(Ordering::SeqCst) >= 1);
}

#[test]
fn on_windows_it_is_powershell_without_a_profile() {
    let r = rig("windows", missing(), 0, "");
    seed(&r, missing());
    r.setup.install("claude").unwrap();
    let (cmd, args) = r.calls.lock().unwrap()[0].clone();
    assert_eq!(cmd, "powershell");
    assert_eq!(&args[..4], ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"]);
    assert!(args[4].ends_with("claude-install.ps1"));
}

#[test]
fn a_failed_installer_gives_one_line_and_the_page_keeps_it() {
    let r = rig("linux", missing(), 1, "boom");
    seed(&r, missing());
    let err = r.setup.install("claude").unwrap_err();
    assert_eq!(err, "Anthropic's installer stopped: see the log.");
    assert_eq!(r.setup.activity()["last"]["ok"], false);
    assert_eq!(r.setup.activity()["last"]["detail"], err);
}

#[test]
fn the_github_cli_is_fetched_with_no_program_run_and_a_bad_checksum_is_refused() {
    let r = rig("linux", missing(), 0, "");
    seed(&r, missing());
    // The download's bytes are not the pinned build's, so nothing is unpacked.
    let err = r.setup.install("gh").unwrap_err();
    assert!(err.contains("isn't the one GitHub published"), "{err}");
    assert!(r.calls.lock().unwrap().is_empty());
}

#[test]
fn macos_git_starts_the_dialog_then_waits_for_git() {
    let r = rig("macos", missing(), 0, "");
    seed(&r, missing());
    // Checks that answer with git present after the first wait.
    r.setup.install("git").unwrap();
    assert_eq!(
        r.calls.lock().unwrap()[0],
        ("xcode-select".to_string(), vec!["--install".to_string()])
    );
    // The background wait ends by 30 minutes at the latest (the wait is instant here).
    for _ in 0..200 {
        if !r.setup.installing() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let a = r.setup.activity();
    assert_eq!(
        a["last"]["detail"],
        "git still wasn't installed after 30 minutes. Press Install to try again."
    );
}

#[test]
fn macos_git_already_installed_is_not_an_error_and_ends_when_git_answers() {
    let r = rig(
        "macos",
        missing(),
        1,
        "xcode-select: error: command line tools are already installed",
    );
    seed(&r, missing());
    let shared = r.setup.checks.shared();
    *r.hook.lock().unwrap() = Some(Box::new(move || {
        *shared.lock().unwrap() = Some(checks_with(true, false, false, false, false))
    }));
    r.setup.install("git").unwrap();
    for _ in 0..200 {
        if !r.setup.installing() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let a = r.setup.activity();
    assert_eq!(
        (a["last"]["ok"].clone(), a["last"]["detail"].clone()),
        (json!(true), json!("git is installed."))
    );
}

#[test]
fn macos_git_that_cant_start_says_so() {
    let r = rig("macos", missing(), 1, "some other failure");
    seed(&r, missing());
    assert_eq!(
        r.setup.install("git").unwrap_err(),
        "macOS couldn't start installing its command-line tools: see the log."
    );
}

#[test]
fn one_install_at_a_time() {
    let r = rig("macos", missing(), 0, "");
    seed(&r, missing());
    r.setup.state().install = Some(Running {
        tool: "gh".into(),
        action: "install",
        since: "t".into(),
        note: None,
    });
    assert_eq!(
        r.setup.install_refusal("claude").as_deref(),
        Some("Another install is running: wait for it.")
    );
    assert_eq!(r.setup.activity()["running"]["tool"], "gh");
}

#[test]
fn os_release_gives_the_id_and_its_likes_and_linux_git_has_one_command() {
    let text = "NAME=\"Ubuntu\"\nID=ubuntu\nID_LIKE=\"debian\"\n";
    assert_eq!(
        parse_os_release(Some(text)),
        Some(("ubuntu".into(), vec!["debian".into()]))
    );
    assert_eq!(parse_os_release(Some("NAME=x\n")), None);
    assert_eq!(linux_git_command(None), "sudo apt install git");
    assert_eq!(
        linux_git_command(parse_os_release(Some("ID=fedora\n")).as_ref()),
        "sudo dnf install git"
    );
    assert_eq!(
        linux_git_command(parse_os_release(Some("ID=endeavouros\nID_LIKE=arch\n")).as_ref()),
        "sudo pacman -S git"
    );
}

#[test]
fn a_sign_in_is_refused_when_missing_signed_in_or_using_an_api_key() {
    let r = rig("linux", missing(), 0, "");
    let env = Env::empty();
    seed(&r, checks_with(true, false, false, false, false));
    assert_eq!(
        r.setup.sign_in_refusal("gh", &env).as_deref(),
        Some("Install it first.")
    );
    assert_eq!(
        r.setup.sign_in_refusal("claude", &env).as_deref(),
        Some("Install it first.")
    );
    seed(&r, checks_with(true, true, true, true, true));
    assert_eq!(
        r.setup.sign_in_refusal("gh", &env).as_deref(),
        Some("Already signed in.")
    );
    assert_eq!(
        r.setup.sign_in_refusal("claude", &env).as_deref(),
        Some("Already signed in.")
    );
    seed(&r, checks_with(true, false, true, true, false));
    assert_eq!(r.setup.sign_in_refusal("gh", &env), None);
    let key = Env::from_pairs([("ANTHROPIC_API_KEY", "k")]);
    assert_eq!(
        r.setup.sign_in_refusal("claude", &key).as_deref(),
        Some("This computer uses an API key.")
    );
    r.setup.state().install = Some(Running {
        tool: "gh".into(),
        action: "install",
        since: "t".into(),
        note: None,
    });
    assert_eq!(
        r.setup.sign_in_refusal("gh", &env).as_deref(),
        Some("Another install is running: wait for it.")
    );
    r.setup.state().install = None;
    seed(&r, missing());
    r.setup.state().sign_in = Some(Running {
        tool: "gh".into(),
        action: "sign_in",
        since: "t".into(),
        note: None,
    });
    assert_eq!(
        r.setup.sign_in_refusal("claude", &env).as_deref(),
        Some("Another sign-in is running: wait for it.")
    );
    assert_eq!(
        r.setup.install_refusal("claude").as_deref(),
        Some("A sign-in is running: wait for it.")
    );
    assert_eq!(r.setup.signing_in(), Some("GitHub"));
}

#[test]
fn gh_prints_a_one_time_code_and_claude_a_link_only_on_anthropics_hosts() {
    assert_eq!(
        device_code("! First copy your one-time code: ABCD-1234\nPress Enter").as_deref(),
        Some("ABCD-1234")
    );
    assert_eq!(
        device_code("\x1b[1mFirst copy your one-time code: AB12-CD34\x1b[0m").as_deref(),
        Some("AB12-CD34")
    );
    assert_eq!(device_code("nothing yet"), None);
    assert_eq!(
        claude_link("Open https://claude.ai/oauth/authorize?code=true to sign in").as_deref(),
        Some("https://claude.ai/oauth/authorize?code=true")
    );
    assert_eq!(
        claude_link("see https://evil.example/claude.ai and https://console.anthropic.com/x").as_deref(),
        Some("https://console.anthropic.com/x")
    );
    assert_eq!(claude_link("https://claude.ai.evil.example/x"), None);
}

#[test]
fn the_code_prompt_is_recognised() {
    assert!(sign_in::asks_for_code_for_test("Paste code here if prompted >"));
    assert!(sign_in::asks_for_code_for_test("Please paste the authorization code"));
    assert!(!sign_in::asks_for_code_for_test("Opening browser to sign in"));
}

#[cfg(unix)]
#[test]
fn a_sign_in_that_runs_shows_its_code_and_ends_with_how_it_went() {
    // A fake `gh` on PATH that prints the code and exits 0.
    let r = rig("linux", missing(), 0, "");
    seed(&r, checks_with(true, false, true, true, true));
    let dir = tempfile::tempdir().unwrap();
    crate::test_support::write_executable(
        &dir.path().join("gh"),
        "#!/bin/sh\necho 'First copy your one-time code: WXYZ-9876'\nexit 0\n",
    );
    let old = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{old}", dir.path().display());
    // The real PATH is read by the spawn, so put it there for the call.
    use crate::secrets::Host;
    crate::secrets::ProcessHost.set("PATH", &path);
    r.setup.sign_in("gh", &Env::empty()).unwrap();
    for _ in 0..300 {
        if r.setup.signing_in().is_none() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    crate::secrets::ProcessHost.set("PATH", &old);
    let a = r.setup.activity();
    assert_eq!(a["last"]["action"], "sign_in");
    assert_eq!(a["last"]["ok"], true);
    assert_eq!(a["last"]["detail"], "Signed in to GitHub.");
    // `gh auth setup-git` ran after it, as an argument list.
    assert_eq!(
        r.calls.lock().unwrap()[0],
        ("gh".to_string(), vec!["auth".to_string(), "setup-git".to_string()])
    );
}
