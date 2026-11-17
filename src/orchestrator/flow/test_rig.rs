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
            let out = crate::spawn::command("git")
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
            config: None,
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

/// A fake `gh` over files in the rig's folder: `pr view` prints `view`, `pr list` prints `gh_list`, a commit's statuses and check
/// runs are `statuses` and `runs` (one JSON object a line), `pr merge` exits `merge-code` (default 0) printing `merge-err`, and
/// `run view` prints a log line. Every call is appended to `gh.log`.
fn ci_gh_script(root: &std::path::Path) -> String {
    let d = root.display();
    format!(
        r#"#!/bin/sh
[ -n "$1" ] || exit 0
echo "$*" >> "{d}/gh.log"
case "$*" in
  "pr view"*isInMergeQueue*) echo 'Unknown JSON field: "isInMergeQueue"' >&2; exit 1 ;;
  "pr view"*) [ -f "{d}/view-err" ] && {{ cat "{d}/view-err" >&2; exit 1; }}; cat "{d}/view" ;;
  "pr list"*) cat "{d}/gh_list" ;;
  "pr comment"*) cat >> "{d}/gh_comment" ;;
  "pr merge"*) [ -f "{d}/merge-err" ] && cat "{d}/merge-err" >&2; exit $(cat "{d}/merge-code" 2>/dev/null || echo 0) ;;
  "api repos/o/p/commits/"*"/status?"*) cat "{d}/statuses" 2>/dev/null ;;
  "api repos/o/p/commits/"*"/check-runs?"*) cat "{d}/runs" 2>/dev/null ;;
  "pr checks"*) cat "{d}/checks_json" 2>/dev/null ;;
  "run view"*) echo "error: boom in the build" ;;
esac
exit 0
"#
    )
}

impl Rig {
    /// Installs the CI-reading fake `gh`, with the change open at `head` and mergeable.
    pub fn ci_gh(&self, head: &str) {
        crate::test_support::write_executable(&self.ctx.gh, &ci_gh_script(&self.root));
        self.view("OPEN", head, "MERGEABLE");
    }

    /// What `gh pr view` answers now.
    pub fn view(&self, state: &str, head: &str, mergeable: &str) {
        let text = format!(
            r#"{{"state":"{state}","headRefOid":"{head}","baseRefName":"main","mergeable":"{mergeable}","mergeStateStatus":"CLEAN","createdAt":"2020-01-01T00:00:00Z","mergeCommit":{{"oid":"mc1"}}}}"#
        );
        std::fs::write(self.root.join("view"), text).unwrap();
    }

    /// The check runs the head shows: `(name, status, conclusion)`, all ended at `ended`.
    pub fn runs(&self, runs: &[(&str, &str, Option<&str>)], ended: &str) {
        let lines: Vec<String> = runs
            .iter()
            .map(|(n, s, c)| {
                format!(
                    r#"{{"name":"{n}","status":"{s}","conclusion":{},"html_url":"https://github.com/o/p/actions/runs/42/job/{n}","completed_at":"{ended}"}}"#,
                    c.map_or("null".into(), |c| format!("\"{c}\""))
                )
            })
            .collect();
        std::fs::write(self.root.join("runs"), lines.join("\n")).unwrap();
    }

    /// What `gh pr checks --json` answers: each check's bucket (`pending`, `cancel`, `fail`, `pass`), behind a GitHub Actions run.
    pub fn checks_json(&self, buckets: &[(&str, &str)]) {
        let rows: Vec<String> = buckets
            .iter()
            .map(|(n, b)| {
                format!(r#"{{"name":"{n}","bucket":"{b}","link":"https://github.com/o/p/actions/runs/42/job/{n}"}}"#)
            })
            .collect();
        std::fs::write(self.root.join("checks_json"), format!("[{}]", rows.join(","))).unwrap();
    }

    /// The calls the fake `gh` took, without the bare run that `write_executable` makes to check the script starts.
    pub fn gh_calls(&self) -> Vec<String> {
        let log = std::fs::read_to_string(self.root.join("gh.log")).unwrap_or_default();
        log.lines().filter(|l| !l.is_empty()).map(String::from).collect()
    }

    /// A workflow on `origin/main` that runs for pull requests into main, so CI is waited for.
    pub fn with_ci(&mut self) {
        let repo = std::path::PathBuf::from(&self.ctx.project.repo_path);
        std::fs::create_dir_all(repo.join(".github/workflows")).unwrap();
        std::fs::write(
            repo.join(".github/workflows/ci.yml"),
            "on:\n  pull_request:\n    branches: [main]\njobs: {}\n",
        )
        .unwrap();
        for args in [
            ["add", "-A"].as_slice(),
            &["commit", "-qm", "ci"],
            &["push", "-q", "origin", "main"],
        ] {
            let out = crate::spawn::command("git")
                .current_dir(&repo)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        }
        self.ctx.project.require_ci = true;
    }
}
