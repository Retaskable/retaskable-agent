//! A real repo with its origin, a lane on the item's branch and a fake `gh`, for the flow's steps.
use std::path::PathBuf;

use super::Ctx;
use crate::config::CheckConfig;
use crate::env::Env;
use crate::item_lifecycle::{ItemRun, ItemState};
use crate::local_model::LocalModel;
use crate::orchestrator::session::tests::{fake_gh, project_in};
use crate::orchestrator::steps::StepRequest;
use crate::store::FlowRecord;

pub(super) const BRANCH: &str = "retaskable/a-thing-s1";
pub(super) const PR: &str = "https://github.com/o/p/pull/7";

pub(super) struct Rig {
    _dir: tempfile::TempDir,
    pub root: PathBuf,
    pub ctx: Ctx,
    pub lane: PathBuf,
}

pub(super) fn check(name: &str, command: &str, blocking: bool) -> CheckConfig {
    CheckConfig {
        name: name.into(),
        command: command.into(),
        blocking,
        timeout_sec: 60,
    }
}

/// A project over a repo with an origin, its lane 1 on [`BRANCH`] with one commit, and the PR open for it.
pub(super) fn rig(checks: Vec<CheckConfig>) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let repo = crate::test_support::repo_with_origin(&root);
    let mut project = project_in(&root, &repo);
    project.checks = checks;
    // The baseline's hold is per project id, across tests running side by side.
    project.id = Some(format!("flow-{}", root.file_name().unwrap().to_string_lossy()));
    let gh = root.join("gh.sh");
    fake_gh(&gh, &root);
    // A reviewer session (its prompt says so) is counted and answers from `review<m>.jsonl`, else approves.
    let claude = root.join("claude.sh");
    let script = format!(
        "#!/bin/sh\n[ $# -eq 0 ] && exit 0\nR='{0}'\ncat > \"$R/in.txt\"\nm=$(cat \"$R/count_r\" 2>/dev/null || echo 0); m=$((m+1)); echo $m > \"$R/count_r\"\ncp \"$R/in.txt\" \"$R/reviewer$m.txt\"\nf=\"$R/review$m.jsonl\"; [ -f \"$f\" ] || f=\"$R/review_default.jsonl\"\ncat \"$f\"\n",
        root.display()
    );
    crate::test_support::write_executable(&claude, &script);
    std::fs::write(root.join("review_default.jsonl"), reviewed("approve", "Fine.", "")).unwrap();
    std::fs::write(root.join("gh_list"), format!(r#"[{{"number":7,"url":"{PR}"}}]"#)).unwrap();
    let lane = crate::lanes::prepare(
        &repo,
        std::path::Path::new(&project.worktrees_dir),
        1,
        BRANCH,
        &project.base_branch,
    )
    .unwrap();
    let rig = Rig {
        _dir: dir,
        ctx: Ctx {
            project,
            gh,
            data_dir: root.join("data"),
            choice: Default::default(),
            env: {
                let claude = claude.to_string_lossy().into_owned();
                Env::new(move |k| (k == "RETASKABLE_CLAUDE").then(|| claude.clone()))
            },
            cursor_models: Default::default(),
            local: LocalModel::off(),
            sessions: Default::default(),
        },
        root,
        lane,
    };
    rig.commit("small.rs", "fn small() {}");
    rig
}

impl Rig {
    /// Commits `file` with `text` in the lane.
    pub fn commit(&self, file: &str, text: &str) {
        std::fs::write(self.lane.join(file), text).unwrap();
        for args in [["add", "-A"].as_slice(), &["commit", "-qm", file]] {
            let out = std::process::Command::new("git")
                .current_dir(&self.lane)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        }
    }

    /// The step's request for the item in `state`.
    pub fn request(&self, state: ItemState) -> StepRequest {
        let mut run = ItemRun::new();
        run.state = state;
        StepRequest {
            project: "p".into(),
            item_id: "a".into(),
            run,
            lane: Some(1),
            token: "lease".into(),
            branch: Some(BRANCH.into()),
            flow: FlowRecord::default(),
            item: Some(crate::backlog::Item {
                id: "a".into(),
                title: "A thing".into(),
                slice: 1,
                ..Default::default()
            }),
            risk_profile: None,
        }
    }
}

/// A reviewer's answer: `extra` is more of its `structured_output` fields (a comma first).
pub(super) fn reviewed(verdict: &str, summary: &str, extra: &str) -> String {
    format!(
        r#"{{"type":"result","subtype":"success","is_error":false,"structured_output":{{"verdict":"{verdict}","summary":"{summary}","comments":[{{"body":"rename the thing"}}]{extra}}},"session_id":"r"}}"#
    )
}

impl Rig {
    /// What the `m`th reviewer answers.
    pub fn script_review(&self, m: u32, answer: String) {
        std::fs::write(self.root.join(format!("review{m}.jsonl")), answer).unwrap();
    }

    /// How many reviewer sessions ran.
    pub fn reviews(&self) -> u32 {
        std::fs::read_to_string(self.root.join("count_r"))
            .ok()
            .and_then(|n| n.trim().parse().ok())
            .unwrap_or(0)
    }

    /// The review comments the fake `gh` was given.
    pub fn comment(&self) -> String {
        std::fs::read_to_string(self.root.join("gh_comment")).unwrap_or_default()
    }

    /// The lane's head.
    pub fn head(&self) -> String {
        crate::lanes::head_sha(&self.lane).unwrap()
    }
}
