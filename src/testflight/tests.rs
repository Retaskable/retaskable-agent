//! A build against a fake runner: the refusals in order, the steps and their environment, the build number pinned, the
//! checkout gone on every path, one build at a time, and the log row on disk. Nothing here runs Xcode or npm.
use serde_json::{Value, json};

use super::fake::{BUILD_NUMBER, Fake, NOW, PBXPROJ, Project, SHA, exited};
use super::*;
use crate::promote::DeployLog;
use crate::promote::Stop;

fn log_of(p: &Project) -> DeployLog {
    DeployLog::open(&p.dir().join(crate::promote::log::FILE))
}

async fn go(p: &Project, f: &Fake, log: &DeployLog, env: Environment) -> Result<Outcome, Stop> {
    check_and_build(&p.config, f, log, &p.dir(), &SHA[..9], "ann", env).await
}

async fn refused(p: &Project, f: &Fake) -> String {
    match go(p, f, &log_of(p), Environment::Production).await {
        Err(Stop::Refused(why)) => why,
        other => panic!("should refuse: {other:?}"),
    }
}

/// The program of every step that isn't the checkout's cleanup.
fn programs(f: &Fake) -> Vec<String> {
    f.with(|s| s.steps.iter().map(|x| x.program.clone()).collect())
}

#[test]
fn the_build_number_is_the_utc_time_to_the_minute() {
    assert_eq!(build_number_at(NOW), BUILD_NUMBER);
    assert_eq!(build_number_at(0), "7001010000");
}

#[tokio::test]
async fn the_refusals_come_in_the_typescripts_order_and_nothing_runs_before_they_pass() {
    let mut p = Project::new();
    let mut f = Fake::new();
    // Everything is wrong at once; the first one says so.
    p.config.release.testflight = false;
    f.mac = false;
    f.permission = Err("gh is not signed in".into());
    f.with(|s| s.served_after = None);
    f.with(|s| s.ends.insert("xcodebuild".into(), exited(1, "")));
    assert_eq!(
        refused(&p, &f).await,
        "TestFlight builds are off on this computer: turn them on in its settings."
    );
    p.config.release.testflight = true;
    assert_eq!(
        refused(&p, &f).await,
        "TestFlight builds need a Mac: Xcode archives the app."
    );
    f.mac = true;
    let script = std::path::Path::new(&p.config.repo_path)
        .join("app")
        .join(build_script());
    let kept = std::fs::read(&script).unwrap();
    std::fs::remove_file(&script).unwrap();
    assert_eq!(
        refused(&p, &f).await,
        "This project has no app/ios Xcode project and app/scripts/testflight-ios.sh."
    );
    std::fs::write(&script, kept).unwrap();
    assert_eq!(
        check_and_build(
            &p.config,
            &f,
            &log_of(&p),
            &p.dir(),
            SHA,
            "not a login!",
            Environment::Production
        )
        .await
        .unwrap_err(),
        Stop::Refused("Only the repo's admins can build for TestFlight.".into())
    );
    assert_eq!(
        refused(&p, &f).await,
        "Couldn't ask GitHub whether @ann is an admin of acme/app: gh is not signed in"
    );
    for (says, has) in [("write", "write"), ("none", "no access"), ("", "no access")] {
        f.permission = Ok(says.into());
        assert_eq!(
            refused(&p, &f).await,
            format!("Only the repo's admins can build for TestFlight (@ann has {has}).")
        );
    }
    f.permission = Ok("admin".into());
    // Staging under Production only: its saved address is never built from.
    p.config.deploy.merges_to = Some("production".into());
    assert_eq!(
        go(&p, &f, &log_of(&p), Environment::Staging).await.unwrap_err(),
        Stop::Refused("This project's merges go straight to production: build production.".into())
    );
    p.config.deploy.merges_to = None;
    p.config.deploy.production.as_mut().unwrap().url = Some("  ".into());
    assert_eq!(
        refused(&p, &f).await,
        "Production has no address to check: set it in Settings → Project settings."
    );
    p.config.deploy.production.as_mut().unwrap().url = Some("https://prod.example".into());
    assert_eq!(
        refused(&p, &f).await,
        "Production doesn't say which commit it serves at https://prod.example/version.json."
    );
    f.with(|s| s.served_after = Some("fff0000fff0000".into()));
    assert_eq!(
        refused(&p, &f).await,
        "Production serves fff0000, not abc1234: build that one."
    );
    f.with(|s| s.served_after = Some(SHA.into()));
    assert_eq!(
        refused(&p, &f).await,
        "Xcode isn't ready on this computer: install it and open it once to accept its license."
    );
    // The only thing run before the build was the Xcode check, and nothing was logged.
    assert!(programs(&f).iter().all(|p| p == "xcodebuild"), "{:?}", f.lines());
    assert_eq!(f.with(|s| s.fetches), 0);
    assert!(log_of(&p).pending().is_empty());
    assert!(!p.dir().join(LOG_FILE).exists());
    f.with(|s| s.ends.clear());
    assert_eq!(
        go(&p, &f, &log_of(&p), Environment::Production).await.unwrap().result,
        BuildResult::Submitted
    );
}

#[tokio::test]
async fn the_staging_address_and_its_version_path_are_what_is_asked() {
    let mut p = Project::new();
    p.config.deploy.staging.as_mut().unwrap().version_path = Some("v.json".into());
    assert_eq!(
        version_url_of(&p.config, Environment::Staging).as_deref(),
        Some("https://staging.example/v.json")
    );
    assert_eq!(
        version_url_of(&p.config, Environment::Production).as_deref(),
        Some("https://prod.example/version.json")
    );
    p.config.deploy.staging = None;
    assert_eq!(version_url_of(&p.config, Environment::Staging), None);
}

#[tokio::test]
async fn the_served_commit_is_read_up_to_five_times_and_the_one_answered_most_wins() {
    let p = Project::new();
    let f = Fake::new();
    // A rollout alternates two versions: the first read that matches ends it.
    f.with(|s| s.served.extend([Some("1111111".into()), Some(SHA.into())]));
    assert!(go(&p, &f, &log_of(&p), Environment::Production).await.is_ok());
    assert_eq!(f.with(|s| s.served_calls), 2);

    // None matches: five reads, and what is named is the commit answered most.
    let f = Fake::new();
    f.with(|s| {
        s.served.extend([
            Some("bbbbbbb".into()),
            Some("aaaaaaa".into()),
            Some("aaaaaaa".into()),
            None,
            Some("bbbbbbb".into()),
        ])
    });
    assert_eq!(
        refused(&p, &f).await,
        "Production serves bbbbbbb, not abc1234: build that one.",
        "a tie goes to the one answered last"
    );
    assert_eq!(f.with(|s| s.served_calls), 5);
}

#[tokio::test]
async fn the_steps_run_in_order_in_a_detached_checkout_with_the_fixed_commands() {
    let p = Project::new();
    let f = Fake::new();
    let out = go(&p, &f, &log_of(&p), Environment::Production).await.unwrap();
    assert_eq!((out.result, out.error.as_deref()), (BuildResult::Submitted, None));
    assert_eq!((out.sha.as_str(), out.build_number.as_str()), (SHA, BUILD_NUMBER));
    let dir = p.checkout();
    let app = dir.join("app");
    assert_eq!(f.with(|s| s.fetches), 1, "fetched before the checkout is made");
    assert_eq!(
        f.lines(),
        [
            "xcodebuild -version".to_string(),
            format!("git worktree add --detach {} {SHA}", dir.display()),
            "npm ci".into(),
            format!("bash {}", build_script().display()),
            format!("git worktree remove --force {}", dir.display()),
        ]
    );
    let steps = f.with(|s| s.steps.clone());
    assert_eq!(steps[1].cwd, std::path::PathBuf::from(&p.config.repo_path));
    assert_eq!((steps[2].cwd.clone(), steps[2].timeout), (app.clone(), Some(INSTALL)));
    assert_eq!((steps[3].cwd.clone(), steps[3].timeout), (app, Some(BUILD)));
    assert_eq!(steps[2].env, []);
    // The production build points at the script's own default origin.
    assert_eq!(
        steps[3].env,
        [("RETASKABLE_BUILD_NUMBER".to_string(), BUILD_NUMBER.to_string())]
    );
}

const INSTALL: std::time::Duration = std::time::Duration::from_secs(10 * 60);
const BUILD: std::time::Duration = std::time::Duration::from_secs(60 * 60);

#[tokio::test]
async fn a_staging_build_also_tells_the_script_stagings_address_without_its_trailing_slash() {
    let p = Project::new();
    let f = Fake::new();
    f.with(|s| s.served_after = Some(SHA.into()));
    go(&p, &f, &log_of(&p), Environment::Staging).await.unwrap();
    assert_eq!(
        f.step("bash").unwrap().env,
        [
            ("RETASKABLE_BUILD_NUMBER".to_string(), BUILD_NUMBER.to_string()),
            (
                "RETASKABLE_CLOUD_ORIGIN".to_string(),
                "https://staging.example".to_string()
            ),
        ]
    );
    let line = &log_of(&p).pending()[0].entry;
    assert_eq!(line["environment"], "staging");
}

#[test]
fn the_build_number_replaces_every_current_project_version_and_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join(xcodeproj());
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, PBXPROJ).unwrap();
    pin_build_number(dir.path(), "2311142213").unwrap();
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "buildSettings = {\n\tCURRENT_PROJECT_VERSION = 2311142213;\n\tMARKETING_VERSION = 1.2;\n};\n\
         buildSettings = {\n\tCURRENT_PROJECT_VERSION = 2311142213;\n};\n"
    );
    // No such project: an error, not a silent skip.
    assert!(pin_build_number(&dir.path().join("none"), "1").is_err());
}

#[tokio::test]
async fn the_number_is_pinned_in_the_checkout_just_before_the_script_and_never_in_the_repo() {
    let p = Project::new();
    let f = Fake::new();
    go(&p, &f, &log_of(&p), Environment::Production).await.unwrap();
    let seen = f.with(|s| s.pbxproj_at_script.clone()).unwrap();
    assert_eq!(seen.matches("CURRENT_PROJECT_VERSION = 2311142213;").count(), 2);
    assert!(!seen.contains("= 7;"));
    let committed =
        std::fs::read_to_string(std::path::Path::new(&p.config.repo_path).join("app").join(xcodeproj())).unwrap();
    assert_eq!(committed, PBXPROJ, "the project's own copy is untouched");
}

#[tokio::test]
async fn a_project_the_number_cannot_be_pinned_in_fails_where_the_typescript_said_submitted() {
    let p = Project::new();
    let f = Fake::new();
    f.with(|s| s.bare_checkout = true);
    let out = go(&p, &f, &log_of(&p), Environment::Production).await.unwrap();
    assert_eq!(out.result, BuildResult::Failed);
    assert!(out.error.is_some());
    assert!(f.step("bash").is_none(), "the script is never run unpinned");
}

#[tokio::test]
async fn the_checkout_is_removed_on_success_failure_timeout_and_cancel() {
    for (end, want) in [
        (None, BuildResult::Submitted),
        (Some(("npm", exited(1, "npm ERR! a\nnpm ERR! b"))), BuildResult::Failed),
        (Some(("bash", exited(65, "** ARCHIVE FAILED **"))), BuildResult::Failed),
        (
            Some((
                "bash",
                super::Ran {
                    timed_out: true,
                    ..Default::default()
                },
            )),
            BuildResult::Failed,
        ),
        (
            Some((
                "bash",
                super::Ran {
                    cut: true,
                    ..Default::default()
                },
            )),
            BuildResult::Cancelled,
        ),
        (Some(("git", exited(128, "fatal: no such commit"))), BuildResult::Failed),
    ] {
        let p = Project::new();
        let f = Fake::new();
        if let Some((program, ran)) = end {
            f.with(|s| s.ends.insert(program.into(), ran));
        }
        let out = go(&p, &f, &log_of(&p), Environment::Production).await.unwrap();
        assert_eq!(out.result, want);
        assert!(!p.checkout().exists(), "{want:?} left its checkout");
        assert!(
            f.lines().last().unwrap().starts_with("git worktree remove --force"),
            "{:?}",
            f.lines()
        );
        assert!(!testflight_running_for(&p), "the place is free again");
    }
}

fn testflight_running_for(p: &Project) -> bool {
    Running::take(std::path::Path::new(&p.config.repo_path)).is_none()
}

#[tokio::test]
async fn a_checkout_git_cannot_remove_is_taken_away_and_pruned_all_the_same() {
    let p = Project::new();
    let f = Fake::new();
    f.with(|s| s.keep_checkout = true);
    go(&p, &f, &log_of(&p), Environment::Production).await.unwrap();
    assert!(!p.checkout().exists());
    assert!(f.lines().last().unwrap().ends_with("worktree prune"));
}

#[tokio::test]
async fn a_failure_says_the_last_lines_and_a_timeout_says_which_command_timed_out() {
    let p = Project::new();
    let f = Fake::new();
    let long = format!("{}\nl1\nl2\nl3\nl4\nl5", "x".repeat(600));
    f.with(|s| s.ends.insert("npm".into(), exited(1, &long)));
    let out = go(&p, &f, &log_of(&p), Environment::Production).await.unwrap();
    assert_eq!(out.error.as_deref(), Some("l1\nl2\nl3\nl4\nl5"));
    // One long line is cut to 500 characters.
    let f = Fake::new();
    f.with(|s| s.ends.insert("npm".into(), exited(1, &"x".repeat(600))));
    let out = go(&p, &f, &log_of(&p), Environment::Production).await.unwrap();
    assert_eq!(out.error.map(|e| e.len()), Some(500));
    // The script is never run after a failed install.
    assert!(f.step("bash").is_none());

    let f = Fake::new();
    f.with(|s| {
        s.ends.insert(
            "bash".into(),
            super::Ran {
                timed_out: true,
                ..Default::default()
            },
        )
    });
    let out = go(&p, &f, &log_of(&p), Environment::Production).await.unwrap();
    assert_eq!(out.error.as_deref(), Some("`bash` timed out."));

    let f = Fake::new();
    f.with(|s| s.fetch_error = Some("couldn't reach origin".into()));
    let out = go(&p, &f, &log_of(&p), Environment::Production).await.unwrap();
    assert_eq!(
        (out.result, out.error.as_deref()),
        (BuildResult::Failed, Some("couldn't reach origin"))
    );
    assert!(
        f.lines().iter().any(|l| l.contains("worktree remove")),
        "cleaned up all the same"
    );
}

#[tokio::test]
async fn only_one_build_runs_at_a_time_in_a_repo_and_the_second_is_refused_with_nothing_run() {
    let p = Project::new();
    let held = Running::take(std::path::Path::new(&p.config.repo_path)).unwrap();
    let f = Fake::new();
    assert_eq!(
        refused(&p, &f).await,
        "A TestFlight build is already running: wait for it."
    );
    assert!(f.step("git").is_none() && f.step("npm").is_none() && f.step("bash").is_none());
    assert!(testflight_running());
    drop(held);
    go(&p, &f, &log_of(&p), Environment::Production).await.unwrap();
    // The build itself held the place while the script ran (the fake looked).
    assert!(f.with(|s| s.running_at_script));
}

#[tokio::test]
async fn the_line_is_in_the_file_and_the_outbox_on_disk_before_anything_is_sent() {
    let p = Project::new();
    let f = Fake::new();
    let log = log_of(&p);
    f.with(|s| s.ends.insert("npm".into(), exited(1, "boom")));
    go(&p, &f, &log, Environment::Production).await.unwrap();
    let want = json!({
        "at": "2023-11-14T22:13:20.000Z", "sha": SHA, "buildNumber": BUILD_NUMBER, "login": "ann",
        "via": "site", "environment": "production", "result": "failed", "error": "boom",
    });
    let in_file = std::fs::read_to_string(p.dir().join(LOG_FILE)).unwrap();
    assert_eq!(serde_json::from_str::<Value>(in_file.trim()).unwrap(), want);
    let queued = log.pending();
    assert_eq!((queued.len(), queued[0].kind.as_str()), (1, "testflight"));
    assert_eq!(Value::Object(queued[0].entry.clone()), want);
    let on_disk: Value =
        serde_json::from_str(&std::fs::read_to_string(p.dir().join(crate::promote::log::FILE)).unwrap()).unwrap();
    assert_eq!(on_disk[0]["kind"], "testflight");
    assert_eq!(on_disk[0]["entry"], want);
    // The build's own output file is made too, with each command shown.
    assert!(
        std::fs::read_to_string(p.dir().join(BUILD_LOG_FILE))
            .unwrap()
            .contains("$ npm ci")
    );
}

#[tokio::test]
async fn a_submitted_build_leaves_no_error_in_its_line() {
    let p = Project::new();
    let log = log_of(&p);
    go(&p, &Fake::new(), &log, Environment::Production).await.unwrap();
    let line = &log.pending()[0].entry;
    assert_eq!(line["result"], "submitted");
    assert!(line.get("error").is_none());
}
