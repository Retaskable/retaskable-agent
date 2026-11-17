//! A steered session against a stub that behaves as Claude Code does with an open input.
#![cfg(unix)]
use std::path::Path;

use super::*;
use crate::engines::claude::ClaudeEngine;
use crate::engines::engine::test_spec;
use crate::engines::{Watch, run_session_steered};
use crate::test_support::write_executable;

const REPLAY: &str = r#"echo '{"type":"user","isReplay":true,"message":{"role":"user","content":"x"}}'"#;

fn result(summary: &str) -> String {
    format!(
        r#"echo '{{"type":"result","subtype":"success","is_error":false,"structured_output":{{"summary":"{summary}"}},"session_id":"s"}}'"#
    )
}

/// A stub `claude` that runs `steps` after reading its prompt line, then waits for its input to close.
fn stub(dir: &Path, steps: &[String]) -> std::path::PathBuf {
    let bin = dir.join("claude.sh");
    let script = format!(
        "#!/bin/sh\necho \"$@\" > '{}/args.txt'\nread prompt\necho \"$prompt\" > '{}/prompt.txt'\n{}\ncat >/dev/null\n",
        dir.display(),
        dir.display(),
        steps.join("\n")
    );
    write_executable(&bin, &script);
    bin
}

async fn run(dir: &Path, bin: &Path, steer: Steer) -> SessionResult {
    let spec = test_spec(dir, "start here");
    run_session_steered(&ClaudeEngine, bin, &spec, "known", None, Watch::default(), steer)
        .await
        .unwrap()
}

fn events(mut heard: mpsc::UnboundedReceiver<SteerEvent>) -> Vec<SteerEvent> {
    let mut out = Vec::new();
    while let Ok(e) = heard.try_recv() {
        out.push(e);
    }
    out
}

fn summary(r: &SessionResult) -> &str {
    r.structured.as_ref().and_then(|s| s["summary"].as_str()).unwrap_or("")
}

#[tokio::test]
async fn a_message_taken_as_the_session_works_is_one_turn_and_closes_the_input_at_its_result() {
    let dir = tempfile::tempdir().unwrap();
    let steps = [REPLAY.into(), "read second".into(), REPLAY.into(), result("both")];
    let bin = stub(dir.path(), &steps);
    let (handle, heard, steer) = channel();
    assert!(handle.send("and also this"));
    let r = run(dir.path(), &bin, steer).await;
    assert_eq!(summary(&r), "both");
    assert_eq!(
        events(heard),
        [SteerEvent::Taken],
        "one message taken beyond the prompt, no turn ended before the last"
    );
    assert!(
        !handle.is_open() && !handle.send("too late"),
        "the input closed at the last result"
    );
    let prompt = std::fs::read_to_string(dir.path().join("prompt.txt")).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(prompt.trim()).unwrap(),
        json!({ "type": "user", "message": { "role": "user", "content": "start here" } }),
        "the prompt goes in as a message"
    );
    let args = std::fs::read_to_string(dir.path().join("args.txt")).unwrap();
    assert!(
        args.contains("--input-format stream-json --replay-user-messages"),
        "{args}"
    );
}

#[tokio::test]
async fn a_message_taken_after_the_turn_ends_is_a_turn_of_its_own_and_the_first_one_is_told() {
    let dir = tempfile::tempdir().unwrap();
    let steps = [
        REPLAY.into(),
        "read second".into(),
        result("first"),
        REPLAY.into(),
        result("second"),
    ];
    let bin = stub(dir.path(), &steps);
    let (handle, heard, steer) = channel();
    assert!(handle.send("next"));
    let r = run(dir.path(), &bin, steer).await;
    assert_eq!(summary(&r), "second", "the session's result is its last turn's");
    let heard = events(heard);
    assert!(
        matches!(&heard[..], [SteerEvent::Turn(first), SteerEvent::Taken] if summary(first) == "first"),
        "{heard:?}"
    );
}

#[tokio::test]
async fn with_nothing_sent_the_input_closes_at_the_first_result() {
    let dir = tempfile::tempdir().unwrap();
    let bin = stub(dir.path(), &[REPLAY.into(), result("only")]);
    let (handle, heard, steer) = channel();
    let r = run(dir.path(), &bin, steer).await;
    assert_eq!(summary(&r), "only");
    assert!(events(heard).is_empty() && !handle.is_open());
}

#[test]
fn a_message_is_a_stream_json_user_line_and_only_a_replay_is_a_take() {
    assert_eq!(
        serde_json::from_str::<Value>(message_line("hi \"there\"\nnew line").trim_end()).unwrap(),
        json!({ "type": "user", "message": { "role": "user", "content": "hi \"there\"\nnew line" } })
    );
    assert!(message_line("x").ends_with('\n') && message_line("a\nb").matches('\n').count() == 1);
    assert!(is_taken(r#"{"type":"user","isReplay":true}"#));
    assert!(!is_taken(r#"{"type":"user","message":{}}"#) && !is_taken(r#"{"type":"assistant","isReplay":true}"#));
    assert!(!is_taken("not json"));
}
