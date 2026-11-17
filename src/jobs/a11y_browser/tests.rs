//! No real browser: `sh` plays the node script, answering the requests in order (the first is `launch`, id 1).
use std::time::{Duration, Instant};

use serde_json::json;

use super::*;
use crate::jobs::a11y_judge::walk_checks;
use crate::jobs::a11y_sweep::{BrowserSweeper, PASSES, SweepRun, Sweeper};

/// A launcher whose process reads one request per answer in `answers` and writes that answer, then reads and
/// discards everything else until stdin ends.
fn fake(answers: &[&str]) -> PlaywrightLauncher {
    let mut script = String::new();
    for a in answers {
        // An answer starting with `!` is written without waiting for a request.
        match a.strip_prefix('!') {
            Some(a) => script.push_str(&format!("echo '{a}'; ")),
            None => script.push_str(&format!("read -r line; echo '{a}'; ")),
        }
    }
    script.push_str("while read -r line; do :; done");
    PlaywrightLauncher::with_program("sh", vec!["-c".into(), script.into()])
}

const LAUNCHED: &str = r#"{"id":1,"result":{}}"#;

#[tokio::test]
async fn a_pass_is_one_request_and_one_answer() {
    let l = fake(&[LAUNCHED, r#"{"id":2,"result":{"passed":["a","b"],"failed":["c"]}}"#]);
    let b = l.launch("http://x", None).await.unwrap();
    let r = b.run_pass("/r", &PASSES[0]).await.unwrap();
    assert_eq!(r.passed, ["a", "b"]);
    assert_eq!(r.failed, ["c"]);
}

#[tokio::test]
async fn the_requests_are_json_lines_with_a_running_id_and_never_shell_text() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("log");
    let script = format!(
        "read -r line; echo \"$line\" >> '{0}'; echo '{LAUNCHED}'; read -r line; echo \"$line\" >> '{0}'; \
         echo '{{\"id\":2,\"result\":\"t\"}}'; while read -r line; do :; done",
        log.display()
    );
    let l = PlaywrightLauncher::with_program("sh", vec!["-c".into(), script.into()]);
    let prefs = json!({"a": "it's \"quoted\" $(x)"});
    let b = l.launch("http://127.0.0.1:1", Some(&prefs)).await.unwrap();
    assert_eq!(b.title("/a b?x='1'").await.unwrap(), "t");
    let lines: Vec<serde_json::Value> = std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(
        lines[0],
        json!({"id":1,"method":"launch","params":{"base":"http://127.0.0.1:1","samplePrefs":prefs}})
    );
    assert_eq!(
        lines[1],
        json!({"id":2,"method":"title","params":{"route":"/a b?x='1'"}})
    );
}

#[tokio::test]
async fn a_pass_sends_its_size_theme_and_what_axe_runs() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("log");
    let script = format!(
        "read -r line; echo '{LAUNCHED}'; read -r line; echo \"$line\" > '{}'; \
         echo '{{\"id\":2,\"result\":{{\"passed\":[],\"failed\":[]}}}}'; while read -r line; do :; done",
        log.display()
    );
    let l = PlaywrightLauncher::with_program("sh", vec!["-c".into(), script.into()]);
    let b = l.launch("http://x", None).await.unwrap();
    b.run_pass("/r", &PASSES[1]).await.unwrap();
    let sent: serde_json::Value = serde_json::from_str(std::fs::read_to_string(&log).unwrap().trim()).unwrap();
    assert_eq!(
        sent["params"],
        json!({"route":"/r","size":"phone","theme":"dark","runOnly":{"type":"rule","values":["color-contrast"]}})
    );
    let full = &PASSES[0];
    assert!(matches!(full.run_only, RunOnly::Tags(_)));
}

#[tokio::test]
async fn an_error_answer_is_the_calls_error() {
    let l = fake(&[
        LAUNCHED,
        r#"{"id":2,"error":{"kind":"other","message":"net::ERR_CONNECTION_REFUSED"}}"#,
    ]);
    let b = l.launch("http://x", None).await.unwrap();
    assert_eq!(b.reflow("/").await.unwrap_err(), "net::ERR_CONNECTION_REFUSED");
}

#[tokio::test]
async fn a_launch_that_found_no_chrome_says_so() {
    let l = fake(&[r#"{"id":1,"error":{"kind":"no_chrome","message":"Executable missing at /x"}}"#]);
    let e = l.launch("http://x", None).await.err().unwrap();
    assert_eq!(
        e,
        LaunchError {
            no_chrome: true,
            message: "Executable missing at /x".into()
        }
    );
}

#[tokio::test]
async fn another_launch_failure_is_not_no_chrome() {
    let l = fake(&[r#"{"id":1,"error":{"kind":"other","message":"crashed"}}"#]);
    let e = l.launch("http://x", None).await.err().unwrap();
    assert!(!e.no_chrome);
    assert_eq!(e.message, "crashed");
}

#[tokio::test]
async fn missing_modules_with_nowhere_to_install_them_is_a_plain_failure() {
    let l = fake(&[r#"{"id":1,"error":{"kind":"modules","message":"Cannot find module 'playwright-core'"}}"#]);
    let e = l.launch("http://x", None).await.err().unwrap();
    assert!(!e.no_chrome);
    assert!(e.message.contains("playwright-core"));
}

#[tokio::test]
async fn a_missing_program_is_a_launch_failure_not_no_chrome() {
    let l = PlaywrightLauncher::with_program("retaskable-no-such-node", Vec::new());
    let e = l.launch("http://x", None).await.err().unwrap();
    assert!(!e.no_chrome);
    assert!(e.message.contains("starting node"), "{}", e.message);
}

#[tokio::test]
async fn a_process_that_ends_fails_the_call_with_what_it_said() {
    let l = PlaywrightLauncher::with_program(
        "sh",
        vec![
            "-c".into(),
            format!("read -r line; echo '{LAUNCHED}'; read -r line; echo 'boom: out of memory' >&2; exit 3").into(),
        ],
    );
    let b = l.launch("http://x", None).await.unwrap();
    let e = b.title("/").await.unwrap_err();
    assert!(e.contains("process ended") && e.contains("out of memory"), "{e}");
    // And a call after it ended fails at once.
    assert!(b.title("/").await.unwrap_err().contains("ended"));
}

#[tokio::test]
async fn a_call_that_is_never_answered_times_out() {
    let l = fake(&[LAUNCHED]);
    let b = l.launch("http://x", None).await.unwrap();
    let e = b
        .rpc
        .call_within("title", json!({}), Duration::from_millis(50))
        .await
        .unwrap_err();
    assert_eq!(e.kind, "timeout");
}

#[tokio::test]
async fn unreadable_lines_and_answers_for_nothing_are_skipped() {
    let l = fake(&[
        LAUNCHED,
        "!not json",
        r#"!{"id":99,"result":1}"#,
        r#"{"id":2,"result":"fine"}"#,
    ]);
    let b = l.launch("http://x", None).await.unwrap();
    assert_eq!(b.title("/").await.unwrap(), "fine");
}

#[tokio::test]
async fn reflow_and_spacing_read_the_pages_numbers() {
    let l = fake(&[
        LAUNCHED,
        r#"{"id":2,"result":{"scrollWidth":400,"clientWidth":320}}"#,
        r#"{"id":3,"result":{"before":[],"after":[{"element":4,"role":"p","name":"Hi","ellipsis":true}]}}"#,
    ]);
    let b = l.launch("http://x", None).await.unwrap();
    assert_eq!(b.reflow("/").await.unwrap(), (400.0, 320.0));
    let (before, after) = b.spacing("/").await.unwrap();
    assert!(before.is_empty());
    assert_eq!(
        after,
        [Overflow {
            element: 4,
            role: "p".into(),
            name: "Hi".into(),
            ellipsis: true
        }]
    );
}

const STYLES: &str = r#"{"outlineStyle":"none","outlineWidth":"0px","outlineColor":"c","boxShadow":"none","borderColor":"b","backgroundColor":"g"}"#;

fn press(element: &str) -> String {
    format!(
        r#"{{"step":{{"element":{element},"role":"a","name":"Home","box":{{"x":1,"y":2,"width":3,"height":4}},"inViewport":true,"focused":{STYLES},"hits":[{{"onElement":true,"role":"a","name":"Home"}},null]}},"prev":null}}"#
    )
}

#[tokio::test]
async fn a_walker_presses_tab_and_reads_the_step() {
    let p = press("1");
    let l = fake(&[
        LAUNCHED,
        r#"{"id":2,"result":{"page":7}}"#,
        r#"{"id":3,"result":[{"element":1,"role":"a","name":"Home"}]}"#,
        &format!(r#"{{"id":4,"result":{p}}}"#),
    ]);
    let b = l.launch("http://x", None).await.unwrap();
    let mut w = b.walker("/", false).await.unwrap();
    assert_eq!(w.page, 7);
    let c = w.candidates().await.unwrap();
    assert_eq!(c[0].name, "Home");
    let pressed = w.press_tab().await.unwrap();
    assert_eq!(pressed.step.element, Some(1));
    assert_eq!(pressed.step.boxed.height, 4.0);
    assert!(pressed.step.in_viewport);
    assert_eq!(pressed.step.focused, pressed.step.unfocused);
    assert_eq!(pressed.step.hits.len(), 2);
    assert!(pressed.step.hits[0].as_ref().unwrap().on_element);
    assert!(pressed.step.hits[1].is_none());
    assert!(pressed.prev.is_none());
}

#[tokio::test]
async fn a_walker_that_reaches_nothing_leaves_the_checks_out() {
    let page = r#"{"step":{"element":null,"role":"page","name":"","box":{"x":0,"y":0,"width":0,"height":0},"inViewport":true,"focused":STYLES,"hits":[]},"prev":null}"#
        .replace("STYLES", STYLES);
    let l = fake(&[
        LAUNCHED,
        r#"{"id":2,"result":{"page":1}}"#,
        r#"{"id":3,"result":[]}"#,
        &format!(r#"{{"id":4,"result":{page}}}"#),
        r#"{"id":5,"result":null}"#,
    ]);
    let b = l.launch("http://x", None).await.unwrap();
    let mut w = b.walker("/", false).await.unwrap();
    assert!(walk_checks(&mut w).await.unwrap().is_empty());
}

/// Polls until `done` (the runtime keeps running meanwhile), failing after a deadline.
async fn poll(what: &str, mut done: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(Instant::now() < until, "gave up waiting for {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn dropping_a_walker_closes_its_page() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("log");
    let script = format!(
        "read -r line; echo '{LAUNCHED}'; read -r line; echo '{{\"id\":2,\"result\":{{\"page\":5}}}}'; \
         while read -r line; do echo \"$line\" >> '{}'; done",
        log.display()
    );
    let l = PlaywrightLauncher::with_program("sh", vec!["-c".into(), script.into()]);
    let b = l.launch("http://x", None).await.unwrap();
    drop(b.walker("/", true).await.unwrap());
    poll("the page close", || {
        std::fs::read_to_string(&log).is_ok_and(|t| t.contains(r#""method":"walkClose""#) && t.contains(r#""page":5"#))
    })
    .await;
}

fn alive(pid: u32) -> bool {
    // `/proc` is the cheap check on Linux; a zombie that is reaped is gone from it.
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn the_process_and_its_children_die_when_the_browser_drops() {
    // The shell starts a child of its own (sleep) and answers `launch` with both pids.
    let script =
        "sleep 1000 & read -r line; echo \"{\\\"id\\\":1,\\\"result\\\":{\\\"pid\\\":$$,\\\"child\\\":$!}}\"; wait";
    let rpc = Rpc::spawn({
        let mut c = crate::spawn::tokio_command("sh");
        c.args(["-c", script]);
        c
    })
    .unwrap();
    let v = rpc.call("launch", json!({})).await.unwrap();
    let (pid, child) = (v["pid"].as_u64().unwrap() as u32, v["child"].as_u64().unwrap() as u32);
    assert!(alive(pid) && alive(child));
    drop(rpc);
    poll("the process tree to die", || !alive(pid) && !alive(child)).await;
}

#[tokio::test]
async fn a_sweep_through_the_launcher_reports_no_chrome() {
    let l = fake(&[r#"{"id":1,"error":{"kind":"no_chrome","message":"nope"}}"#]);
    let run = BrowserSweeper(l).sweep("http://x", &["/".to_string()], None).await;
    assert_eq!(
        run,
        SweepRun::Failed {
            no_chrome: true,
            error: "nope".into()
        }
    );
}

#[test]
fn the_script_parses_as_javascript() {
    let ok = crate::spawn::command("node")
        .args([
            "-e",
            "new (require('node:vm').Script)(require('node:fs').readFileSync(process.argv[1],'utf8'))",
        ])
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/a11y-browser.cjs"))
        .output();
    match ok {
        Ok(o) => assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr)),
        Err(_) => eprintln!("no node here; skipped"),
    }
}

/// Needs node with `playwright-core` and `axe-core` on NODE_PATH (or network for the install) and a Chrome, and a
/// site at `A11Y_SMOKE_BASE` (default http://127.0.0.1:8080). Run with `--ignored`.
#[tokio::test]
#[ignore]
async fn smoke_a_real_chrome_sweeps_a_real_page() {
    let tools = tempfile::tempdir().unwrap();
    let base = std::env::var("A11Y_SMOKE_BASE").unwrap_or_else(|_| "http://127.0.0.1:8080".into());
    let run = BrowserSweeper(PlaywrightLauncher::new(tools.path().join("a11y-tools")))
        .sweep(&base, &["/".to_string()], None)
        .await;
    match run {
        SweepRun::Ok { result, .. } => {
            assert_eq!(result.routes, 1);
            assert!(!result.rules.is_empty());
            assert!(result.checks.iter().any(|c| c.id == "scan-page-title"));
        }
        SweepRun::Failed { error, .. } => panic!("{error}"),
    }
}
