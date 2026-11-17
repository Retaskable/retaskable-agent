//! The base branch ("main") as the store sees it (docs/specs/pools.md "The main look"; `agent/src/mainLook.ts` in the
//! TypeScript agent, pools-on-server.md section 15). This computer reports what it knows of the branch in `health`'s
//! `main` (its head, the baseline and CI results it holds for it, how far behind the default branch it is) and runs the
//! `main` job the store sends when it wants a look. It decides nothing: not when main is red, not when to look, not
//! whether a fix or a sync follows. The store derives those and sends the tasks.
//!
//! [`MainLook`] is what the computer holds (shared between the loop, which fills it after each fetch and reads it for
//! `health`, and the job, which fills the results); [`main_job`] is the job. Everything in a fact is checked before it
//! is sent ([`MainFacts`]): the store refuses a `main` that does not fit, and that would fail the whole `health`.
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::{Value, json};

use crate::baseline::{Baseline, baseline};
use crate::checks::CheckRun;
use crate::ci_coverage::{ci_coverage, read_workflows, workflows_tree};
use crate::jobs::{JobOutcome, JobSpec, Outcome};
use crate::kept_facts::{BASELINE, CI_COVERAGE, KeptFacts, MAIN_CI};
use crate::main_ci::{self, MainCi, MainCiState};
use crate::sync::lock;
use crate::text::tail;

/// What the store takes of a name and of a list of them, and of a job's brief (`mainLook.ts`).
const NAME_MAX: usize = 300;
const NAMES_MAX: usize = 50;
pub const BRIEF_MAX: usize = 20_000;
/// The most of one failing check's output the brief keeps.
const CHECK_TAIL: usize = 3000;

/// A commit as the store checks it: 7 to 64 hex digits.
pub fn is_sha(s: &str) -> bool {
    (7..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// `s` cut to `max` UTF-16 units, as the store counts a text.
fn cut_units(s: &str, max: usize) -> String {
    let mut used = 0;
    let mut out = String::new();
    for c in s.chars() {
        used += c.len_utf16();
        if used > max {
            break;
        }
        out.push(c);
    }
    out
}

/// The names the store takes: not empty, each cut short, a bounded list.
fn names<'a>(list: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    (list.into_iter())
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(|n| cut_units(n, NAME_MAX))
        .take(NAMES_MAX)
        .collect()
}

/// The default branch has commits the base branch lacks, and its head.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Behind {
    pub branch: String,
    pub head: String,
    pub count: u32,
}

/// The integration-branch pattern's way of being behind (it catches up by a rebase, not a sync): the default branch `branch`, at
/// `head`, has `count` commits the base branch (at `base`) lacks, and `waiting` names the items and plans cut from the base
/// branch that have not gone into it yet (the rebase would rewrite their history). Facts only: the store decides when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Rebase {
    pub branch: String,
    pub head: String,
    pub base: String,
    pub count: u32,
    pub waiting: Vec<String>,
}

/// The last baseline run: the blocking checks that failed in it, at the commit it ran on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BaselineFact {
    pub sha: String,
    pub failed: Vec<String>,
}

/// The last CI read from the code host: `pass`, `pending`, `fail` or `none`, with the failed checks' names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CiFact {
    pub sha: String,
    pub state: &'static str,
    pub failed: Vec<String>,
}

/// The key of the CI coverage this computer holds: the base branch and the workflows' tree it was read at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CoverageKey {
    pub base: String,
    pub tree: Option<String>,
}

/// What a fetch showed of the base branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    pub branch: String,
    pub sha: String,
    pub tree: Option<String>,
    pub behind: Option<Behind>,
    /// Instead of `behind`, for a project that catches up by a rebase; `waiting` is read live, when the facts are made.
    pub rebase: Option<Rebase>,
}

/// `health`'s `main`: facts of the base branch after the last fetch (pools-on-server.md section 15.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MainFacts {
    pub branch: String,
    pub sha: String,
    pub tree: Option<String>,
    /// The base checkout is in use (a play, a phone build, a sync, a baseline), so no baseline can run now.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub busy: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub behind: Option<Behind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rebase: Option<Rebase>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline: Option<BaselineFact>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ci: Option<CiFact>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage: Option<CoverageKey>,
}

#[derive(Default)]
struct Held {
    seen: Option<Seen>,
    baseline: Option<BaselineFact>,
    ci: Option<CiFact>,
    coverage: Option<CoverageKey>,
    /// What waits to go into the base branch now, as the loop last read it (a rebase would rewrite its history).
    waiting: Vec<String>,
}

/// What this computer holds of the base branch for one project. Clones share it.
#[derive(Clone, Default)]
pub struct MainLook(Arc<Mutex<Held>>);

impl MainLook {
    /// What a fetch showed. Anything in it that does not fit the store's shape is left out, not sent.
    pub fn see(&self, seen: Seen) {
        let behind = seen
            .behind
            .filter(|b| is_sha(&b.head) && !b.branch.is_empty() && b.branch.encode_utf16().count() <= 200);
        let rebase = seen.rebase.filter(|r| {
            is_sha(&r.head) && is_sha(&r.base) && !r.branch.is_empty() && r.branch.encode_utf16().count() <= 200
        });
        lock(&self.0).seen = Some(Seen { behind, rebase, ..seen });
    }

    /// What waits to go into the base branch, read live by the loop: the rebase is held back while anything does.
    pub fn set_waiting(&self, waiting: Vec<String>) {
        lock(&self.0).waiting = waiting;
    }

    pub fn waiting(&self) -> Vec<String> {
        lock(&self.0).waiting.clone()
    }

    /// The base branch's head after the last fetch; `None` before the first.
    pub fn head(&self) -> Option<String> {
        lock(&self.0).seen.as_ref().map(|s| s.sha.clone())
    }

    /// Holds the baseline run at `sha` and answers what it says of it.
    pub fn hold_baseline(&self, sha: &str, runs: &[CheckRun]) -> BaselineFact {
        let failed = names(
            runs.iter()
                .filter(|r| r.blocking && !r.passed())
                .map(|r| r.name.as_str()),
        );
        let fact = BaselineFact {
            sha: sha.to_string(),
            failed,
        };
        lock(&self.0).baseline = Some(fact.clone());
        fact
    }

    pub fn hold_ci(&self, ci: &MainCi) {
        lock(&self.0).ci = Some(ci_fact(ci));
    }

    pub fn hold_coverage(&self, base: &str, tree: Option<String>) {
        lock(&self.0).coverage = Some(CoverageKey {
            base: base.to_string(),
            tree,
        });
    }

    /// The facts for `health`: `None` before the first fetch, and when what was seen is not one the store would take.
    /// `busy`: the base checkout is in use.
    pub fn facts(&self, busy: bool) -> Option<MainFacts> {
        let held = lock(&self.0);
        let seen = held.seen.as_ref()?;
        if seen.branch.is_empty() || seen.branch.encode_utf16().count() > 200 || !is_sha(&seen.sha) {
            return None;
        }
        let tree_ok = |t: &Option<String>| t.as_ref().is_none_or(|t| !t.is_empty() && t.len() <= 100);
        Some(MainFacts {
            branch: seen.branch.clone(),
            sha: seen.sha.clone(),
            tree: seen.tree.clone().filter(|t| tree_ok(&Some(t.clone()))),
            busy: busy.then_some(true),
            behind: seen.behind.clone(),
            rebase: seen.rebase.clone().map(|r| Rebase {
                waiting: names(held.waiting.iter().map(String::as_str)),
                ..r
            }),
            baseline: held.baseline.clone().filter(|b| is_sha(&b.sha)),
            ci: held.ci.clone().filter(|c| is_sha(&c.sha)),
            coverage: held
                .coverage
                .clone()
                .filter(|c| !c.base.is_empty() && c.base.encode_utf16().count() <= 200 && tree_ok(&c.tree)),
        })
    }
}

/// A CI result as the store takes it: a failure that names no check is `none` (nothing to fix).
fn ci_fact(ci: &MainCi) -> CiFact {
    let failed = names(ci.failed.iter().map(|f| f.name.as_str()));
    let state = match ci.state {
        MainCiState::Pass => "pass",
        MainCiState::Pending => "pending",
        MainCiState::Fail if failed.is_empty() => "none",
        MainCiState::Fail => "fail",
        MainCiState::None => "none",
    };
    CiFact {
        sha: ci.sha.clone(),
        state,
        failed: if state == "fail" { failed } else { Vec::new() },
    }
}

/// What a `main` job needs of the project.
pub struct Look<'a> {
    pub id: &'a str,
    pub project: &'a crate::config::ProjectConfig,
    pub held: &'a MainLook,
    pub facts: &'a KeptFacts,
    pub gh: &'a Path,
}

/// The refusal for a look this computer does not know.
const UNKNOWN: &str = "This computer doesn't know that look at main.";

/// `ref` of `branch` on origin, as the last fetch left it; `None` when it is not there.
pub fn head_of(repo: &Path, branch: &str) -> Option<String> {
    let at = format!("origin/{branch}^{{commit}}");
    crate::lanes::git_output_in_worktree(repo, &["rev-parse", "--verify", "--quiet", &at])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The `main` job: reads what the store wants read at the commit it names (the host's CI, whether CI covers the branch, or
/// the baseline's checks) and answers the facts, with the output of what failed. It does not run for a commit main has
/// moved past: that answers the bare `{ sha }`.
pub async fn main_job(look: &Look<'_>, spec: &JobSpec) -> JobOutcome {
    let (Some(want), Some(sha)) = (spec.arg_str("want"), spec.arg_str("sha")) else {
        return Outcome::refused(UNKNOWN);
    };
    if look.project.repo_path.trim().is_empty() {
        return Outcome::refused("This project isn't set up here.");
    }
    let repo = PathBuf::from(&look.project.repo_path);
    let base = look.project.base_branch.clone();
    let (r, b) = (repo.clone(), base.clone());
    let head = tokio::task::spawn_blocking(move || head_of(&r, &b))
        .await
        .ok()
        .flatten();
    let sha = sha.to_string();
    if head.as_deref() != Some(sha.as_str()) {
        return Outcome::done(json!({ "sha": sha }));
    }
    match want {
        "rebase" => rebase(look, &repo, &base, &sha, spec).await,
        "coverage" => coverage(look, &repo, &base, &sha).await,
        "ci" => ci(look, &repo, &sha).await,
        "baseline" => baseline_look(look, &repo, &base, &sha).await,
        _ => Outcome::refused(UNKNOWN),
    }
}

/// Whether CI runs for pull requests into the branch, kept as the store's `ci_coverage` fact (GitHub projects only: another
/// host writes no row rather than a wrong `none`), and the key it was read at, which tells the store it need not ask again.
async fn coverage(look: &Look<'_>, repo: &Path, base: &str, sha: &str) -> JobOutcome {
    let reads = main_ci::reads(look.project);
    let (r, b) = (repo.to_path_buf(), base.to_string());
    let read = tokio::task::spawn_blocking(move || {
        let at = format!("origin/{b}");
        let tree = workflows_tree(&r, &at);
        let fact = reads.then(|| ci_coverage(&read_workflows(&r, &at), &b).to_fact(&b, tree.as_deref()));
        (tree, fact)
    })
    .await;
    let Ok((tree, fact)) = read else {
        return Outcome::failed("The workflows could not be read.");
    };
    if let Some(fact) = fact {
        look.facts.keep_result(CI_COVERAGE, &fact);
    }
    look.held.hold_coverage(base, tree);
    Outcome::done(json!({ "sha": sha }))
}

/// The host's CI on `sha`, with the failed checks' logs for the brief; refused when the host cannot say.
async fn ci(look: &Look<'_>, repo: &Path, sha: &str) -> JobOutcome {
    if !main_ci::reads(look.project) {
        return Outcome::refused("The code host can't say what CI found.");
    }
    let (gh, dir, github, at) = (
        look.gh.to_path_buf(),
        repo.to_path_buf(),
        look.project.github_repo.clone(),
        sha.to_string(),
    );
    let read = tokio::task::spawn_blocking(move || {
        let ci = main_ci::read(&gh, &dir, &github, &at, &|_| {});
        let brief = if ci.failed.is_empty() {
            String::new()
        } else {
            main_ci::brief(&at, &ci.failed, &main_ci::logs(&gh, &dir, &github, &ci.failed, &|_| {}))
        };
        (ci, brief)
    })
    .await;
    let Ok((ci, brief)) = read else {
        return Outcome::failed("The code host's CI could not be read.");
    };
    look.facts.keep_result(MAIN_CI, &ci.to_fact());
    look.held.hold_ci(&ci);
    let fact = ci_fact(&ci);
    let mut result = json!({ "sha": sha, "ci": fact });
    if fact.state == "fail" && !brief.is_empty() {
        result["brief"] = json!(tail(&brief, BRIEF_MAX));
    }
    Outcome::done(result)
}

/// The project's checks run in the base checkout: `busy` when something else has it, a failure in words when they could not run.
async fn baseline_look(look: &Look<'_>, repo: &Path, base: &str, sha: &str) -> JobOutcome {
    let p = look.project;
    // Only the commands a person approved on this computer run (the gate has already left the rest out): with none, nothing
    // ran, and that is not a green main.
    if p.checks.is_empty() {
        return Outcome::refused("No checks are approved to run on this computer yet.");
    }
    let (id, worktrees, branch, checks, at) = (
        look.id.to_string(),
        PathBuf::from(&p.worktrees_dir),
        base.to_string(),
        p.checks.clone(),
        sha.to_string(),
    );
    let dir = repo.to_path_buf();
    let ran = tokio::task::spawn_blocking(move || baseline(&id, &dir, &worktrees, &branch, &at, &checks)).await;
    let runs = match ran {
        Ok(Ok(Baseline::Done(runs))) => runs,
        // Main moved on while the checkout was made ready: what the store asked about is over.
        Ok(Ok(Baseline::Stale { .. })) => return Outcome::done(json!({ "sha": sha })),
        Ok(Ok(Baseline::Busy)) => return Outcome::done(json!({ "sha": sha, "busy": true })),
        Ok(Err(e)) => {
            tracing::warn!("{}: the baseline didn't run: {e:#}", look.id);
            return Outcome::failed("The base branch's checks could not be run.");
        }
        Err(e) => {
            tracing::warn!("{}: the baseline task failed: {e}", look.id);
            return Outcome::failed("The base branch's checks could not be run.");
        }
    };
    look.facts.keep_result(BASELINE, &baseline_fact(sha, &runs));
    let held = look.held.hold_baseline(sha, &runs);
    let mut result = json!({ "sha": sha, "baseline": held });
    if !held.failed.is_empty() {
        result["brief"] = json!(tail(&baseline_brief(sha, &runs), BRIEF_MAX));
    }
    Outcome::done(result)
}

/// What the fix session is told of the failing baseline checks: each one's last lines.
fn baseline_brief(sha: &str, runs: &[CheckRun]) -> String {
    let fence = "```";
    (runs.iter().filter(|r| r.blocking && !r.passed()))
        .map(|r| {
            format!(
                "{} failed on main at {}:\n\n{fence}\n{}\n{fence}",
                r.name,
                crate::backlog::runs::short_sha(sha),
                tail(r.output_tail.trim_end(), CHECK_TAIL)
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The store's `baseline` fact: `{ sha, results: [{ name, blocking, ok, durationSec }], at }`. The checks' output is
/// left out: a fact holds derived results only (spec 03.16), and the brief a fix main needs goes in the job's result.
pub fn baseline_fact(sha: &str, runs: &[CheckRun]) -> Value {
    json!({
        "sha": sha,
        "results": runs.iter().map(|r| json!({
            "name": r.name,
            "blocking": r.blocking,
            "ok": r.passed(),
            "durationSec": r.seconds,
        })).collect::<Vec<_>>(),
        "at": crate::telemetry::events::iso_utc_millis(crate::telemetry::events::now_ms()),
    })
}

/// The `rebase` look (github-workflow.md section 4.2): rebases the integration branch onto the main commit the store named and
/// pushes it with a lease on the head it read. The store decided it was due; this only checks what only this computer can: that it
/// still holds the heads the store named (else the bare `{ sha }`: moved on), that nothing waits to go into the branch and nothing
/// else uses the checkout (`busy`, asked again later). A conflict pushes nothing and is answered as the files; why it could not
/// push is answered as the error.
async fn rebase(look: &Look<'_>, repo: &Path, base: &str, sha: &str, spec: &JobSpec) -> JobOutcome {
    use crate::main_rebase::{Rebased, WORKTREE, rebase_onto_main, uses_rebase};
    if !uses_rebase(look.project) {
        return Outcome::refused("This project doesn't catch its base branch up by a rebase.");
    }
    let (Some(main), Some(main_sha)) = (spec.arg_str("main"), spec.arg_str("mainSha")) else {
        return Outcome::refused(UNKNOWN);
    };
    let (r, m) = (repo.to_path_buf(), main.to_string());
    if tokio::task::spawn_blocking(move || head_of(&r, &m))
        .await
        .ok()
        .flatten()
        .as_deref()
        != Some(main_sha)
    {
        return Outcome::done(json!({ "sha": sha }));
    }
    // Nothing waits, and nothing else has the checkout for as long as the rebase takes.
    let hold = crate::base_hold::hold_base(look.id, "a rebase");
    if !look.held.waiting().is_empty() || hold.is_err() {
        return Outcome::done(json!({ "sha": sha, "busy": true }));
    }
    let worktree = PathBuf::from(&look.project.worktrees_dir).join(WORKTREE);
    let (dir, b, mn, bs, ms) = (
        repo.to_path_buf(),
        base.to_string(),
        main.to_string(),
        sha.to_string(),
        main_sha.to_string(),
    );
    let done = tokio::task::spawn_blocking(move || rebase_onto_main(&dir, &worktree, &b, &mn, &bs, &ms)).await;
    drop(hold);
    let outcome = match done {
        Ok(Ok(done)) => done,
        Ok(Err(e)) => return Outcome::failed(format!("Couldn't rebase {base} onto {main}: {e:#}")),
        Err(e) => return Outcome::failed(format!("Couldn't rebase {base} onto {main}: {e}")),
    };
    let rebase = match outcome {
        Rebased::Done => json!({}),
        Rebased::Conflict(files) => json!({ "conflict": names(files.iter().map(String::as_str)) }),
        Rebased::Failed(why) => json!({ "error": why.chars().take(500).collect::<String>() }),
    };
    Outcome::done(json!({ "sha": sha, "rebase": rebase }))
}

#[cfg(test)]
mod tests;
