//! The real runner's own behaviour on small commands (no Xcode): output appended to the build log, the environment
//! passed, a timeout killing the whole tree, and a stop from outside seen as a cut. Unix only (`sh`).
use std::path::Path;
use std::time::{Duration, Instant};

use super::{RealTools, Step, Tools};

fn tools() -> RealTools {
    RealTools {
        repo: ".".into(),
        github_repo: "a/b".into(),
        gh: "gh".into(),
    }
}

fn sh(script: &str, dir: &Path) -> Step {
    Step::new("sh", &["-c", script], dir)
}

#[tokio::test]
async fn output_goes_to_the_build_log_whole_and_the_environment_reaches_the_command() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("build.log");
    let mut step = sh("echo \"n=$RETASKABLE_BUILD_NUMBER\"; echo oops >&2; exit 3", dir.path());
    step.env.push(("RETASKABLE_BUILD_NUMBER".into(), "42".into()));
    let ran = tools().run(&step, Some(&log)).await;
    assert_eq!((ran.code, ran.timed_out, ran.cut), (Some(3), false, false));
    assert_eq!(ran.output, "n=42\noops\n");
    // A second step adds to the file and reads back only its own output.
    let ran = tools().run(&sh("echo two", dir.path()), Some(&log)).await;
    assert!(ran.ok());
    assert_eq!(ran.output, "two\n");
    assert_eq!(std::fs::read_to_string(&log).unwrap(), "n=42\noops\ntwo\n");
}

#[tokio::test]
async fn a_command_that_cannot_start_ends_with_no_code_and_says_why() {
    let dir = tempfile::tempdir().unwrap();
    let ran = tools()
        .run(&Step::new("no-such-program-here", &[], dir.path()), None)
        .await;
    assert_eq!(ran.code, None);
    assert!(
        ran.output.starts_with("starting no-such-program-here"),
        "{}",
        ran.output
    );
}

fn alive(pid: &str) -> bool {
    std::process::Command::new("kill")
        .args(["-0", pid])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[tokio::test]
async fn a_timeout_kills_the_whole_tree_not_just_the_shell() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("pid");
    let mut step = sh("sleep 60 & echo $! > pid; wait", dir.path());
    step.timeout = Some(Duration::from_millis(1500));
    let started = Instant::now();
    let ran = tools().run(&step, None).await;
    assert!(ran.timed_out && ran.code.is_none(), "{ran:?}");
    assert!(started.elapsed() < Duration::from_secs(20));
    let pid = std::fs::read_to_string(pidfile).unwrap().trim().to_string();
    let gone = Instant::now();
    while alive(&pid) && gone.elapsed() < Duration::from_secs(5) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!alive(&pid), "the grandchild outlived the timeout");
}

#[tokio::test]
async fn a_stop_from_outside_is_a_cut_and_not_a_failure_of_the_command() {
    let dir = tempfile::tempdir().unwrap();
    let step = sh("sleep 60", dir.path());
    let stop = async {
        tokio::time::sleep(Duration::from_millis(500)).await;
        crate::process_tree::cut_all();
    };
    let tools = tools();
    let (ran, ()) = tokio::join!(tools.run(&step, None), stop);
    crate::process_tree::reset_for_tests();
    assert!(ran.cut && !ran.timed_out && ran.code.is_none(), "{ran:?}");
}
