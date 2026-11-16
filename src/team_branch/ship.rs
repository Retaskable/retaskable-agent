//! The merge control into main (`shipToMain` in the TS agent's `teamBranchActions.ts`, `notMergedNote`,
//! `restartPlan` and `startAgainFromMain` in `teamBranch.ts`; github-workflow.md sections 4.2 and 8.1): squash the open
//! pull request from the base branch into main as it was read, never deleting the base branch, then start the base
//! branch again from main once GitHub says it merged. GitHub's words only; the merge pattern only (no local-only
//! project and no check that main takes writes, as in the rest of this port; the driver takes the store's merge turn
//! around it, `driver/ship_turn.rs`).
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
    /// The squash commit on main, once merged (`{ "oid": … }` in `gh`'s answer).
    #[serde(default, rename = "mergeCommit")]
    merge_commit: Option<Oid>,
}

#[derive(Debug, Deserialize)]
struct Oid {
    #[serde(default)]
    oid: String,
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
        "state,headRefOid,mergeCommit",
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

/// A pull request from the base into main that merged, as `gh pr list --state merged` says it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MergedPr {
    number: u64,
    #[serde(default)]
    merged_at: Option<String>,
    #[serde(default, rename = "headRefOid")]
    head: String,
}

/// The newest pull request from the base into main that merged, or none.
fn last_merged(gh: &Path, dir: &Path, github_repo: &str, base: &str, main: &str) -> Result<Option<MergedPr>> {
    let args = [
        "pr",
        "list",
        "--repo",
        github_repo,
        "--base",
        main,
        "--head",
        base,
        "--state",
        "merged",
        "--limit",
        "5",
        "--json",
        "number,mergedAt,headRefOid",
    ];
    crate::host_call::guard("gh", dir, &args)?;
    let mut cmd = Command::new(gh);
    cmd.current_dir(dir).args(args);
    let out = crate::implementer::publish::run(cmd, None, "gh pr list")?;
    let merged: Vec<MergedPr> = serde_json::from_str(out.trim()).context("reading gh pr list's answer")?;
    Ok(merged.into_iter().max_by(|a, b| a.merged_at.cmp(&b.merged_at)))
}

/// A merge that landed whose restart didn't (read only): the newest pull request from the base into main merged and
/// origin's base is still at the head it merged, as after a stop or a git failure between `gh pr merge` and
/// `start_again_from_main`. Answers its number and head. Needs a fetch done just before. A base with nothing main
/// lacks (already restarted) is nothing to finish and asks `gh` nothing.
pub fn unfinished_restart(
    gh: &Path,
    repo: &Path,
    github_repo: &str,
    base: &str,
    main: &str,
) -> Result<Option<(u64, String)>> {
    if super::ahead_behind(repo, base, main)?.0 == 0 {
        return Ok(None);
    }
    let Some(merged) = last_merged(gh, repo, github_repo, base, main)? else {
        return Ok(None);
    };
    let at = git(repo, &["rev-parse", &format!("origin/{base}")]).map_err(anyhow::Error::msg)?;
    Ok((!merged.head.is_empty() && at.trim() == merged.head).then_some((merged.number, merged.head)))
}

/// The pull request a ship will squash in, as read: what the store's merge turn is taken for (`merge_enqueue`'s `pr`
/// and `head`) and what `gh pr merge` is matched to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shipping {
    pub number: u64,
    pub url: String,
    pub head: String,
}

/// How a ship ended once the merge was asked for: whether GitHub reports the pull request merged (and its squash commit
/// when it said which), and the warning to give the person, if any (a merge only queued, or a restart skipped or failed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shipped {
    pub merged: bool,
    pub merge_commit: Option<String>,
    pub warning: Option<String>,
}

/// The merge control's first half, after `need_team_branch` said there is a team branch: reads the branch, checks it can
/// go and answers the pull request as it stands. Nothing is changed, so it is read before the store's merge turn is taken
/// (the pull request's number and url go into the queue) and again with the turn held. `items` are the store's.
pub fn read_for_ship(
    gh: &Path,
    repo: &Path,
    github_repo: &str,
    base: &str,
    main: &str,
    items: &[Item],
) -> Result<Shipping, Stop> {
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
    Ok(Shipping {
        number,
        url: open.url.clone(),
        head,
    })
}

/// The merge control's second half: squashes the pull request in as read and starts the base branch again.
pub fn merge_for_ship(
    gh: &Path,
    repo: &Path,
    github_repo: &str,
    base: &str,
    main: &str,
    shipping: &Shipping,
) -> Result<Shipped, Stop> {
    let (number, head) = (shipping.number, &shipping.head);
    if let Err(e) = merge(gh, repo, github_repo, number, head) {
        return Err(refused(format!("GitHub didn't merge PR #{number}: {}", gh_reason(&e))));
    }
    // A failed look is not a merged pull request, so it is asked twice before it reads as not merged. The TS agent
    // reads once (`teamBranchActions.ts:101`) and a hiccup there left the dev branch on its old head; the finisher
    // below (`finish_restart`) also picks that up on a later pass.
    let now = view(gh, repo, github_repo, number).or_else(|_| view(gh, repo, github_repo, number));
    let state = now.as_ref().map_or("UNKNOWN", |v| v.state.as_str());
    if let Some(note) = not_merged_note(state, number, base) {
        return Ok(Shipped {
            merged: false,
            merge_commit: None,
            warning: Some(note),
        });
    }
    let merge_commit = now
        .ok()
        .and_then(|v| v.merge_commit)
        .map(|c| c.oid)
        .filter(|oid| !oid.is_empty());
    Ok(Shipped {
        merged: true,
        merge_commit,
        warning: start_again_from_main(repo, base, main, head),
    })
}

#[cfg(test)]
mod tests;
