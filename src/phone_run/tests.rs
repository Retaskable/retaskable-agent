use std::time::{Duration, Instant};

use super::*;
use crate::test_support::repo_with_origin;

/// The state is the machine's one, so these tests take turns.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

#[cfg(windows)]
const LONG_RUNNING: &str = "ping -n 60 127.0.0.1 >nul";
#[cfg(not(windows))]
const LONG_RUNNING: &str = "sleep 60";

fn pixel() -> Phone {
    Phone {
        serial: "0A1B".into(),
        model: "Pixel 8".into(),
        state: "device".into(),
    }
}

fn config(build: &str) -> PhoneConfig {
    PhoneConfig {
        app_dir: "app".into(),
        build_command: build.into(),
        app_id: "com.example.app".into(),
        item_id: None,
    }
}

fn project(id: &str, root: &Path, build: Option<&str>) -> ProjectView {
    ProjectView {
        id: id.into(),
        github_repo: "o/one".into(),
        backlog_path: "backlog".into(),
        repo_path: repo_with_origin(root),
        worktrees_dir: root.join("worktrees"),
        base_branch: "main".into(),
        play_command: String::new(),
        phone: build.map(config),
        organisation_id: None,
        code_host: "github".into(),
        code_host_ref: String::new(),
    }
}

/// Starts from no phones, no run and the given phones listed.
fn fresh(phones: Vec<Phone>) {
    *lock(&STATE) = State::default();
    publish_devices(Path::new("adb"), &Ok(phones));
}

fn run_of(project: &ProjectView, dir: &Path) -> Value {
    view_json(project, dir).unwrap()["run"].clone()
}

fn wait_for_step(project: &ProjectView, dir: &Path, step: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let run = run_of(project, dir);
        if run["step"] == step || Instant::now() > deadline {
            return run;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_project_without_a_phone_section_is_refused_and_has_no_phone_view() {
    let _turn = lock(&ONE_AT_A_TIME);
    fresh(vec![pixel()]);
    let root = tempfile::tempdir().unwrap();
    let p = project("ph-none", root.path(), None);
    assert_eq!(
        play("ph-none", &p, root.path(), "0A1B", true, false).unwrap_err(),
        "Play on phone isn't set up."
    );
    assert!(view_json(&p, root.path()).is_none());
}

#[test]
fn a_phone_that_is_not_listed_or_not_authorised_is_refused() {
    let _turn = lock(&ONE_AT_A_TIME);
    fresh(vec![
        pixel(),
        Phone {
            serial: "LOCKED".into(),
            model: "Old".into(),
            state: "unauthorized".into(),
        },
    ]);
    let root = tempfile::tempdir().unwrap();
    let p = project("ph-refuse", root.path(), Some("echo built"));
    for serial in ["nope", "LOCKED"] {
        assert_eq!(
            play("ph-refuse", &p, root.path(), serial, true, false).unwrap_err(),
            "That phone isn't connected with USB debugging allowed."
        );
    }
    assert_eq!(
        play("ph-refuse", &p, root.path(), "0A1B", false, true).unwrap_err(),
        "Uninstalling needs confirmed: true."
    );
    assert!(!busy("ph-refuse"));
}

#[test]
fn the_first_install_to_a_phone_shows_the_warning_and_a_confirmed_one_is_remembered() {
    let _turn = lock(&ONE_AT_A_TIME);
    fresh(vec![pixel()]);
    let root = tempfile::tempdir().unwrap();
    let p = project("ph-warn", root.path(), Some("exit 1"));
    let asked = play("ph-warn", &p, root.path(), "0A1B", false, false).unwrap_err();
    assert!(asked.starts_with("Install on Pixel 8?\n"), "{asked}");
    assert_eq!(view_json(&p, root.path()).unwrap()["phones"][0]["warned"], false);
    play("ph-warn", &p, root.path(), "0A1B", true, false).unwrap();
    wait_for_step(&p, root.path(), "failed");
    assert_eq!(view_json(&p, root.path()).unwrap()["phones"][0]["warned"], true);
    // Remembered: no confirmation the second time.
    play("ph-warn", &p, root.path(), "0A1B", false, false).unwrap();
    wait_for_step(&p, root.path(), "failed");
}

#[test]
fn a_build_that_fails_shows_its_command_and_output_and_lets_go_of_the_base_checkout() {
    let _turn = lock(&ONE_AT_A_TIME);
    fresh(vec![pixel()]);
    let root = tempfile::tempdir().unwrap();
    let p = project("ph-build", root.path(), Some("echo no gradle here && exit 3"));
    play("ph-build", &p, root.path(), "0A1B", true, false).unwrap();
    let run = wait_for_step(&p, root.path(), "failed");
    let message = run["message"].as_str().unwrap();
    assert!(
        message.starts_with("`echo no gradle here && exit 3` failed (3):\n") && message.contains("no gradle here"),
        "{message}"
    );
    assert_eq!(
        (run["serial"].as_str(), run["model"].as_str()),
        (Some("0A1B"), Some("Pixel 8"))
    );
    // The hold is given back right after the step is shown.
    let deadline = Instant::now() + Duration::from_secs(30);
    while crate::base_hold::base_holder("ph-build").is_some() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(crate::base_hold::base_holder("ph-build").is_none());
    assert!(!busy("ph-build"));
}

#[test]
fn a_build_that_works_but_has_no_capacitor_cli_fails_saying_so() {
    let _turn = lock(&ONE_AT_A_TIME);
    fresh(vec![pixel()]);
    let root = tempfile::tempdir().unwrap();
    let p = project("ph-cli", root.path(), Some("echo built"));
    play("ph-cli", &p, root.path(), "0A1B", true, false).unwrap();
    let run = wait_for_step(&p, root.path(), "failed");
    assert_eq!(
        run["message"],
        "Capacitor's CLI isn't installed in app/ (node_modules/@capacitor/cli)."
    );
}

#[test]
fn a_second_run_is_refused_while_one_builds_and_phone_stop_ends_it() {
    let _turn = lock(&ONE_AT_A_TIME);
    fresh(vec![pixel()]);
    let root = tempfile::tempdir().unwrap();
    let p = project("ph-stop", root.path(), Some(LONG_RUNNING));
    play("ph-stop", &p, root.path(), "0A1B", true, false).unwrap();
    assert!(busy("ph-stop"));
    assert_eq!(
        play("ph-stop", &p, root.path(), "0A1B", true, false).unwrap_err(),
        "Play on phone is already running."
    );
    assert_eq!(run_of(&p, root.path())["step"], "building");
    // Not through its own build yet: wait for the process to be there to cut.
    let deadline = Instant::now() + Duration::from_secs(30);
    while lock(&STATE).running.as_ref().is_none_or(|r| lock(&r.tree).is_none()) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    stop();
    assert_eq!(wait_for_step(&p, root.path(), "stopped")["step"], "stopped");
    assert!(!busy("ph-stop"));
    // Stopping when nothing runs is not an error.
    stop();
}

#[test]
fn another_holder_of_the_base_checkout_refuses_a_run() {
    let _turn = lock(&ONE_AT_A_TIME);
    fresh(vec![pixel()]);
    let root = tempfile::tempdir().unwrap();
    let p = project("ph-held", root.path(), Some("echo built"));
    let hold = crate::base_hold::hold_base("ph-held", "the baseline").unwrap();
    assert_eq!(
        play("ph-held", &p, root.path(), "0A1B", true, false).unwrap_err(),
        "The base checkout is busy: the baseline is using it."
    );
    drop(hold);
}

#[test]
fn the_view_lists_the_phones_with_adbs_status_and_the_apps_id() {
    let _turn = lock(&ONE_AT_A_TIME);
    fresh(vec![]);
    let root = tempfile::tempdir().unwrap();
    let mut p = project("ph-view", root.path(), Some("echo built"));
    p.phone.as_mut().unwrap().item_id = Some("android-app".into());
    publish_devices(Path::new("adb"), &Err("adb devices timed out".into()));
    let view = view_json(&p, root.path()).unwrap();
    assert_eq!(
        view,
        json!({ "phones": [], "status": "adb devices timed out.", "appId": "com.example.app", "wip": true })
    );
    publish_devices(Path::new("adb"), &Err("daemon not running".into()));
    assert_eq!(
        view_json(&p, root.path()).unwrap()["status"],
        "adb devices failed: daemon not running"
    );
    adb_missing();
    assert_eq!(view_json(&p, root.path()).unwrap()["status"], phone::NO_ADB);
}

#[test]
fn a_change_in_the_phones_signals_the_snapshot_and_no_change_does_not() {
    let _turn = lock(&ONE_AT_A_TIME);
    fresh(vec![]);
    let mut seen = subscribe();
    seen.mark_unchanged();
    publish_devices(Path::new("adb"), &Ok(vec![]));
    assert!(!seen.has_changed().unwrap());
    publish_devices(Path::new("adb"), &Ok(vec![pixel()]));
    assert!(seen.has_changed().unwrap());
}

#[test]
fn what_a_failed_install_means() {
    assert_eq!(
        install_failure("... INSTALL_FAILED_UPDATE_INCOMPATIBLE: x", "app"),
        (
            "incompatible",
            Some("The phone has the app signed with a different key.".into())
        )
    );
    let (step, message) = install_failure("'gradlew' is not recognized as a command", "app");
    assert_eq!(step, "failed");
    assert!(
        message
            .unwrap()
            .starts_with("Windows couldn't run Gradle's wrapper (app/android/gradlew.bat).")
    );
    let (_, message) = install_failure("SDK location not found. Define sdk.dir", "app");
    assert!(
        message
            .unwrap()
            .starts_with("Android's SDK wasn't found: set ANDROID_HOME")
    );
    let (_, message) = install_failure("boom", "app");
    assert_eq!(message.unwrap(), "Installing failed:\nboom");
}

#[test]
fn capacitors_cli_is_found_in_the_app_folder_or_the_root() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(cap_cli(dir.path(), "app"), None);
    let rel: PathBuf = ["node_modules", "@capacitor", "cli", "bin"].iter().collect();
    std::fs::create_dir_all(dir.path().join(&rel)).unwrap();
    std::fs::write(dir.path().join(&rel).join("capacitor"), "").unwrap();
    assert_eq!(
        cap_cli(dir.path(), "app"),
        Some(dir.path().join(&rel).join("capacitor"))
    );
    let app = dir.path().join("app").join(&rel);
    std::fs::create_dir_all(&app).unwrap();
    std::fs::write(app.join("capacitor"), "").unwrap();
    assert_eq!(cap_cli(dir.path(), "app"), Some(app.join("capacitor")));
}
