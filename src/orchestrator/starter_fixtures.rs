//! What the tests of the read-only run starters (`plan_run`, `look_run`) share: a project whose `claude` is a script,
//! a run request, and what the run sends back.
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::mpsc::UnboundedReceiver;

use super::driver::{RunContext, RunEnd, RunEndSender, RunProcesses, RunRequest};
use super::session::tests::{fake_name, project_in};
use crate::backlog::runs::RunKind;
use crate::config::ProjectConfig;
use crate::env::Env;
use crate::process_tree::ProcessTree;

/// A temp folder holding a project whose `claude` records its input, arguments and folder, then prints `out`
/// (after `extra`, a shell snippet).
pub struct Fixture {
    pub _dir: tempfile::TempDir,
    pub root: PathBuf,
    pub project: ProjectConfig,
    pub env: Env,
}

pub fn fixture(out: &str, extra: &str) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let script = format!(
        "#!/bin/sh\n[ $# -eq 0 ] && exit 0\nR='{r}'\ncat > \"$R/in.txt\"\necho \"$@\" > \"$R/args.txt\"\npwd > \"$R/pwd.txt\"\n{extra}\ncat \"$R/out.jsonl\"\n",
        r = root.display()
    );
    let bin = root.join(fake_name());
    crate::test_support::write_executable(&bin, &script);
    std::fs::write(root.join("out.jsonl"), out).unwrap();
    let bin = bin.to_string_lossy().into_owned();
    let env = Env::new(move |k| (k == "RETASKABLE_CLAUDE").then(|| bin.clone()));
    let project = project_in(&root, &repo);
    Fixture {
        _dir: dir,
        root,
        project,
        env,
    }
}

/// A `claude` result line with this structured output.
pub fn answer(structured: &str) -> String {
    format!(
        r#"{{"type":"result","subtype":"success","is_error":false,"structured_output":{structured},"session_id":"s"}}"#
    )
}

pub struct Started {
    pub ends: UnboundedReceiver<RunEnd>,
    pub trees: UnboundedReceiver<(String, Arc<ProcessTree>)>,
    pub stop: crate::runs::RunStop,
}

pub fn request(kind: RunKind, piece: Value, other: Value) -> (RunRequest, Started) {
    let (ends, rx) = RunEndSender::channel();
    let (processes, trees) = RunProcesses::channel();
    let stop = crate::runs::RunStop::default();
    let request = RunRequest {
        token: "tok".into(),
        project: "p".into(),
        processes,
        stop: stop.clone(),
        kind,
        piece: serde_json::from_value(piece).unwrap(),
        other: other.as_object().cloned().unwrap_or_default(),
        lane: None,
        ends,
        context: RunContext::for_test(),
    };
    (request, Started { ends: rx, trees, stop })
}

pub async fn end_of(rx: &mut UnboundedReceiver<RunEnd>) -> RunEnd {
    tokio::time::timeout(Duration::from_secs(60), rx.recv())
        .await
        .expect("an end")
        .expect("the channel stays open")
}
