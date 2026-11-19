//! The `change_actions` job (no session, no slot): the store asks this computer, which holds the code host's access, to
//! close or re-run CI of changes the agent opened. It decides nothing: each action is checked against the host first (the
//! change must be on a `retaskable/` branch, and still open; a merged change is never touched), then done with the host's
//! CLI. Never merges, never pushes, never deletes a branch. A host error is the action's own error, never a success.
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::{JobOutcome, JobSpec, Outcome};
use crate::config::ProjectConfig;
use crate::implementer::publish::{close_change_by_number, view_change};
use crate::text::truncate;

/// At most this many actions are done in one task, a comment is cut to `COMMENT_MAX` and an error to `ERROR_MAX`.
const ACTIONS_MAX: usize = 10;
const COMMENT_MAX: usize = 500;
const ERROR_MAX: usize = 300;
const DEFAULT_COMMENT: &str = "Closed by Retaskable.";
/// The branches the agent opens its changes on; no other change is touched.
const AGENT_BRANCH: &str = "retaskable/";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Close,
    Rerun,
}

/// One action the store asked.
struct Action {
    id: String,
    kind: Option<Kind>,
    pr: Option<u64>,
    sha: Option<String>,
    comment: String,
}

fn action(v: &Value) -> Option<Action> {
    let id = v.get("id")?.as_str().filter(|i| !i.is_empty())?.to_string();
    let kind = match v.get("kind").and_then(Value::as_str) {
        Some("close") => Some(Kind::Close),
        Some("rerun") => Some(Kind::Rerun),
        _ => None,
    };
    let text = |k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    Some(Action {
        id,
        kind,
        pr: v.get("pr").and_then(Value::as_u64).filter(|n| *n > 0),
        sha: text("sha"),
        comment: truncate(&text("comment").unwrap_or_else(|| DEFAULT_COMMENT.into()), COMMENT_MAX),
    })
}

/// What one action came to: done, skipped on purpose (still `ok`), or refused or failed.
enum Done {
    Did,
    Skipped(&'static str),
    Refused(&'static str),
    Failed(String),
}

/// The one line `done` carries for an error.
fn one_line(why: &str) -> String {
    let line = why.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("failed");
    truncate(line, ERROR_MAX)
}

/// How the actions reach the code host: tests hand a fake.
pub trait Host: Send + Sync {
    fn view(&self, pr: u64) -> anyhow::Result<crate::implementer::publish::ChangeView>;
    fn close(&self, pr: u64, comment: &str) -> anyhow::Result<()>;
    /// Starts CI again on the change's head; false when the host couldn't.
    fn rerun(&self, pr: u64) -> bool;
}

/// GitHub, through `gh` run in the project's checkout.
struct Gh {
    project: ProjectConfig,
    gh: PathBuf,
}

impl Gh {
    fn dir(&self) -> &Path {
        Path::new(&self.project.repo_path)
    }
}

impl Host for Gh {
    fn view(&self, pr: u64) -> anyhow::Result<crate::implementer::publish::ChangeView> {
        view_change(&self.gh, self.dir(), pr)
    }

    fn close(&self, pr: u64, comment: &str) -> anyhow::Result<()> {
        close_change_by_number(&self.gh, self.dir(), pr, comment)
    }

    fn rerun(&self, pr: u64) -> bool {
        crate::ci_pass::host::rerun_forced(&self.project, self.gh.clone(), pr)
    }
}

fn run_one(host: &dyn Host, a: &Action) -> Done {
    let (Some(kind), Some(pr)) = (a.kind, a.pr) else {
        return Done::Refused("not a close or rerun of a change");
    };
    let view = match host.view(pr) {
        Ok(v) => v,
        Err(e) => return Done::Failed(format!("{e:#}")),
    };
    if !view.head_ref_name.starts_with(AGENT_BRANCH) {
        return Done::Refused("not an agent branch");
    }
    if !view.is_open() {
        // Closed or merged: nothing to do, and a merged change is never touched.
        return Done::Skipped("already closed");
    }
    match kind {
        Kind::Close => match host.close(pr, &a.comment) {
            Ok(()) => Done::Did,
            Err(e) => Done::Failed(format!("{e:#}")),
        },
        Kind::Rerun => {
            if a.sha.as_deref().is_some_and(|s| s != view.head_ref_oid) {
                return Done::Skipped("head moved");
            }
            if host.rerun(pr) {
                Done::Did
            } else {
                Done::Failed("the code host couldn't start CI again".into())
            }
        }
    }
}

/// The actions against `host`, one entry each in `done`. Always ok: a failed action is its own entry's error.
pub fn run_actions(spec: &JobSpec, host: &dyn Host) -> JobOutcome {
    let actions = (spec.args.get("actions").and_then(Value::as_array).into_iter().flatten())
        .take(ACTIONS_MAX)
        .filter_map(action);
    let done: Vec<Value> = actions
        .map(|a| match run_one(host, &a) {
            Done::Did => json!({ "id": a.id, "ok": true }),
            Done::Skipped(why) => json!({ "id": a.id, "ok": true, "skipped": why }),
            Done::Refused(why) => json!({ "id": a.id, "ok": false, "skipped": why }),
            Done::Failed(why) => json!({ "id": a.id, "ok": false, "error": one_line(&why) }),
        })
        .collect();
    Outcome::done(json!({ "done": done }))
}

/// The `change_actions` job against the project's real code host.
pub async fn change_actions_job(project: &ProjectConfig, gh: &Path, spec: &JobSpec) -> JobOutcome {
    if project.repo_path.trim().is_empty() {
        return Outcome::refused("This computer has no checkout of the project to act on changes of.");
    }
    if !crate::main_ci::reads(project) {
        return Outcome::refused("This computer can only close and re-run changes on GitHub.");
    }
    let host = Gh {
        project: project.clone(),
        gh: gh.to_path_buf(),
    };
    let spec = spec.clone();
    // Every call runs a program, so on a blocking thread.
    match tokio::task::spawn_blocking(move || run_actions(&spec, &host)).await {
        Ok(out) => out,
        Err(e) => Outcome::failed(format!("the change actions' task crashed: {e}")),
    }
}

#[cfg(test)]
#[path = "change_actions_tests.rs"]
mod tests;
