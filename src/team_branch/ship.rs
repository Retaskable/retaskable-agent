//! The merge control into main (`shipToMain` in the TS agent's `teamBranchActions.ts`, `notMergedNote`,
//! `restartPlan` and `startAgainFromMain` in `teamBranch.ts`; github-workflow.md sections 4.2 and 8.1): squash the open
//! pull request from the base branch into main as it was read, never deleting the base branch, then start the base
//! branch again from main once GitHub says it merged. GitHub's words only; the merge pattern only (no local-only
//! project, no merge queue, and no check that main takes writes, as in the rest of this port).
//!
//! The one place this differs from the TS agent: it answers a warning (a merge that was only queued, or a restart that
//! was skipped or failed) and the caller puts it in the row's result, where the TS agent's HTTP answer carried it to
//! the strip. Nothing here ever pushes to main: the only push names the base branch, and leases on exactly what
//! origin showed.
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::Deserialize;

use super::{TeamBranch, gh_reason, read, ship_refusal};
use crate::backlog::Item;

/// How long each of the restart's git calls may take (`startAgainFromMain`'s `timeoutMs`).
const GIT_TIMEOUT: Duration = Duration::from_secs(120);
/// How much of a failed call's stderr goes into a warning (`cut(…, 300)`).
const CUT: usize = 300;

/// Why shipping stopped before the pull request merged: the TS agent's 409 (`Refused`, plain words) or something that
/// broke (`Failed`: git or `gh` couldn't answer).
#[derive(Debug)]
pub enum Stop {
    Refused(String),
    Failed(anyhow::Error),
}

impl From<anyhow::Error> for Stop {
    fn from(e: anyhow::Error) -> Self {
        Stop::Failed(e)
    }
}

fn refused(why: impl Into<String>) -> Stop {
    Stop::Refused(why.into())
}

/// `gh pr view`'s answer.
#[derive(Debug, Deserialize)]
struct View {
    #[serde(default)]
    state: String,
    #[serde(default, rename = "headRefOid")]
    head: String,
}

/// The pull request as `gh` says it is now.
fn view(gh: &Path, dir: &Path, github_repo: &str, pr: u64) -> Result<View> {
    let number = pr.to_string();
    let args = [
        "pr",
        "view",
        &number,
        "--repo",
        github_repo,
        "--json",
        "state,headRefOid",
    ];
    crate::host_call::guard("gh", dir, &args)?;
    let mut cmd = Command::new(gh);
    cmd.current_dir(dir).args(args);
    let out = crate::implementer::publish::run(cmd, None, "gh pr view")?;
    serde_json::from_str(out.trim()).context("reading gh pr view's answer")
}

/// `gh pr merge <pr> --repo <repo> --squash --match-head-commit <head>`: squashed in as read, and never with
/// `--delete-branch`, so the long-lived dev branch stays and nothing that landed since goes with it.
fn merge(gh: &Path, dir: &Path, github_repo: &str, pr: u64, head: &str) -> Result<()> {
    let number = pr.to_string();
    let args = [
        "pr",
        "merge",
        &number,
        "--repo",
        github_repo,
        "--squash",
        "--match-head-commit",
        head,
    ];
    crate::host_call::guard("gh", dir, &args)?;
    let mut cmd = Command::new(gh);
    cmd.current_dir(dir).args(args);
    crate::implementer::publish::run(cmd, None, &format!("gh pr merge {pr}")).map(|_| ())
}

/// After the merge exits 0: none when GitHub reports the pull request merged, so the base branch can start again from
/// main, else why it was left as it is. `gh pr merge` also exits 0 when it only queued the pull request, and starting
/// again then would throw the queued work away (`notMergedNote`).
pub fn not_merged_note(state: &str, pr: u64, base: &str) -> Option<String> {
    (state != "MERGED").then(|| {
        format!("PR #{pr} didn't go in straight away (GitHub may have queued it), so {base} was left as it is.")
    })
}

/// How the base branch starts again, from where origin's copy of it is now: gone, still at the head that merged, or
/// moved on with work main doesn't have (`restartPlan`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Restart {
    Create,
    Reset,
    Moved,
}

pub fn restart_plan(now: Option<&str>, merged: &str) -> Restart {
    match now {
        None => Restart::Create,
        Some(now) if now == merged => Restart::Reset,
        Some(_) => Restart::Moved,
    }
}

/// What a git call said: its stdout when it exited 0, else its stderr, cut to 300 characters.
fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = crate::git_bin::command(dir);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("starting git: {e}"))?;
    // Read both pipes while it runs, so a chatty child never blocks on a full pipe.
    fn drain(pipe: Option<impl std::io::Read + Send + 'static>) -> std::thread::JoinHandle<String> {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut bytes);
            }
            String::from_utf8_lossy(&bytes).into_owned()
        })
    }
    let (out, err) = (drain(child.stdout.take()), drain(child.stderr.take()));
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if started.elapsed() > GIT_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                break Err("timed out".to_string());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => break Err(e.to_string()),
        }
    };
    let (out, err) = (out.join().unwrap_or_default(), err.join().unwrap_or_default());
    match status {
        Ok(s) if s.success() => Ok(out),
        Ok(_) => Err(err.trim().chars().take(CUT).collect()),
        Err(e) => Err(e),
    }
}

/// Starts the base branch again from main, after its pull request into main was squashed in. The push leases on what
/// origin showed a moment ago, so work that lands in between is never thrown away. A warning, or none
/// (`startAgainFromMain`).
pub fn start_again_from_main(repo: &Path, base: &str, main: &str, merged: &str) -> Option<String> {
    if let Err(why) = git(repo, &["fetch", "origin", main]) {
        return Some(format!("Couldn't fetch {main} to start {base} again: {why}"));
    }
    let branch_ref = format!("refs/heads/{base}");
    let listed = match git(repo, &["ls-remote", "--heads", "origin", &branch_ref]) {
        Ok(out) => out,
        Err(why) => {
            return Some(format!(
                "Couldn't read {base} on GitHub to start it again from {main}: {why}"
            ));
        }
    };
    let now = listed.split_whitespace().next();
    if restart_plan(now, merged) == Restart::Moved {
        return Some(format!(
            "{base} has new work since its pull request was read, so it wasn't started again from {main}."
        ));
    }
    let push = push_args(base, main, now);
    git(repo, &push.iter().map(String::as_str).collect::<Vec<_>>())
        .err()
        .map(|why| format!("Couldn't start {base} again from {main}: {why}"))
}

/// The push that starts the base branch again. The lease names the exact sha origin showed (empty to create), never a
/// bare `--force`, and the destination is the base branch's own ref: main is only ever the source, read from its
/// remote-tracking ref.
fn push_args(base: &str, main: &str, now: Option<&str>) -> Vec<String> {
    let branch_ref = format!("refs/heads/{base}");
    vec![
        "push".into(),
        format!("--force-with-lease={branch_ref}:{}", now.unwrap_or("")),
        "origin".into(),
        format!("refs/remotes/origin/{main}:{branch_ref}"),
    ]
}

/// The merge control, after `need_team_branch` said there is a team branch: reads the branch, checks it can go, squashes
/// the pull request in as read and starts the base branch again. `items` are the store's. Answers the warning to give the
/// person when it merged but the branch wasn't started again (or was only queued), or none when all went well.
pub fn ship(
    gh: &Path,
    repo: &Path,
    github_repo: &str,
    base: &str,
    main: &str,
    items: &[Item],
) -> Result<Option<String>, Stop> {
    crate::lanes::fetch_latest(repo)?;
    let tb: TeamBranch = read(gh, repo, github_repo, base, main, items)?;
    if let Some(why) = ship_refusal(&tb) {
        return Err(refused(why));
    }
    // `ship_refusal` leaves no pull request out only when there is one.
    let Some(open) = &tb.main_pr else {
        return Err(refused(format!("No pull request from {base} into {main} is open.")));
    };
    let (number, now) = (open.number, view(gh, repo, github_repo, open.number)?);
    if now.state != "OPEN" {
        return Err(refused(format!("PR #{number} isn't open any more.")));
    }
    // Merged as the refusal check read it, so a push since then fails the merge instead of going in unreviewed.
    let head = if open.head.is_empty() {
        now.head
    } else {
        open.head.clone()
    };
    if let Err(e) = merge(gh, repo, github_repo, number, &head) {
        return Err(refused(format!("GitHub didn't merge PR #{number}: {}", gh_reason(&e))));
    }
    // A failed look is not a merged pull request: it reads as not merged, as in the TS agent.
    let state = view(gh, repo, github_repo, number).map_or_else(|_| "UNKNOWN".to_string(), |v| v.state);
    if let Some(note) = not_merged_note(&state, number, base) {
        return Ok(Some(note));
    }
    Ok(start_again_from_main(repo, base, main, &head))
}

#[cfg(test)]
mod tests;
