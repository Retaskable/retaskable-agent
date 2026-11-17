//! A real repo with its origin, a lane on the item's branch and a fake `gh`, for the flow's steps.
use std::path::PathBuf;

use super::Ctx;
use crate::config::CheckConfig;
use crate::item_lifecycle::{ItemRun, ItemState};
use crate::orchestrator::session::tests::{fake_gh, project_in};
use crate::orchestrator::steps::StepRequest;
use crate::store::FlowRecord;

pub(super) const BRANCH: &str = "retaskable/a-thing-s1";
pub(super) const PR: &str = "https://github.com/o/p/pull/7";

pub(super) struct Rig {
    _dir: tempfile::TempDir,
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
        ctx: Ctx { project, gh },
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
        }
    }
}
