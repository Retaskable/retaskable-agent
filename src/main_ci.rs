//! Main's own CI on the code host (docs/specs/agent/06.7-chores-never-idle.md, "The baseline"; `agent/src/mainCi.ts`
//! in the TypeScript agent): the host's CI on the base branch's commit is read until it settles, and a failure is a
//! red main the fix-main chore takes on like a failing baseline check. Once per commit: a settled answer is not asked
//! for again until main moves. Only GitHub's host reads commit checks here (as in the TS agent, GitLab's has none).
//! The result is the store's `main_ci` fact; the failing checks' logs are read for the fix session's brief and are
//! never stored.
use std::path::Path;

use serde_json::{Value, json};

use crate::ci_checks::{CommitChecks, CommitState, FailedCheck, ci_logs, commit_checks};
use crate::ci_watch::Reg;
use crate::text::tail;

/// The most failed checks the `main_ci` fact keeps (the store refuses more), and the longest name and link.
const FAILED_MAX: usize = 50;
const NAME_MAX: usize = 300;
const LINK_MAX: usize = 1000;

/// What the host's CI says about a commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MainCiState {
    Pass,
    Pending,
    Fail,
    /// No CI reported, or the host could not say.
    None,
}

impl MainCiState {
    fn name(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Pending => "pending",
            Self::Fail => "fail",
            Self::None => "none",
        }
    }
}

/// The host's CI on one commit of the base branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MainCi {
    pub sha: String,
    pub state: MainCiState,
    /// Only with [`MainCiState::Fail`]: the failed checks, a cancelled run left out.
    pub failed: Vec<FailedCheck>,
}

impl MainCi {
    /// What `checks` (or the reason the host could not say) makes of `sha`. A cancelled run isn't a failure of main's
    /// (the next push runs it again), so a failure of only cancelled runs is `none`.
    pub fn of(sha: &str, checks: std::result::Result<CommitChecks, String>) -> Self {
        let none = |sha: &str| Self {
            sha: sha.to_string(),
            state: MainCiState::None,
            failed: Vec::new(),
        };
        let Ok(checks) = checks else {
            return none(sha);
        };
        match checks.state {
            CommitState::Pass => Self {
                state: MainCiState::Pass,
                ..none(sha)
            },
            CommitState::Pending => Self {
                state: MainCiState::Pending,
                ..none(sha)
            },
            CommitState::None => none(sha),
            CommitState::Fail => {
                let failed: Vec<FailedCheck> = checks.failed.into_iter().filter(|f| !f.cancelled).collect();
                if failed.is_empty() {
                    none(sha)
                } else {
                    Self {
                        state: MainCiState::Fail,
                        failed,
                        ..none(sha)
                    }
                }
            }
        }
    }

    /// Settled for `sha`: read for it and no longer pending, so it is not asked again until main moves.
    pub fn settled_for(&self, sha: &str) -> bool {
        self.sha == sha && self.state != MainCiState::Pending
    }

    /// The failed checks, when this is main's CI at `base_sha` and it failed (`mainCiFailed`); none otherwise.
    pub fn red(&self, base_sha: &str) -> &[FailedCheck] {
        if self.sha == base_sha && self.state == MainCiState::Fail {
            &self.failed
        } else {
            &[]
        }
    }

    /// The `main_ci` fact: `{ sha, state, failed? }`.
    pub fn to_fact(&self) -> Value {
        let mut fact = json!({ "sha": self.sha, "state": self.state.name() });
        if !self.failed.is_empty() {
            fact["failed"] = self
                .failed
                .iter()
                .take(FAILED_MAX)
                .map(|f| {
                    let mut check = json!({
                        "name": f.name.chars().take(NAME_MAX).collect::<String>(),
                        "link": f.link.chars().take(LINK_MAX).collect::<String>(),
                    });
                    if let Some(at) = &f.completed_at {
                        check["completedAt"] = json!(at);
                    }
                    check
                })
                .collect();
        }
        fact
    }
}

/// Reads the host's CI on `sha` of `repo`. A host that can't say (no access, a network blip) is `none`, not an
/// error, and isn't asked again until main moves, so it isn't logged every minute.
pub fn read(gh: &Path, dir: &Path, repo: &str, sha: &str, reg: Reg) -> MainCi {
    let checks = commit_checks(gh, dir, repo, sha, reg).map_err(|e| {
        tracing::info!("main CI: reading CI on {} failed: {e:#}", short(sha));
        e.to_string()
    });
    MainCi::of(sha, checks)
}

/// Each failed check's log, for [`brief`]: `None` where it can't be read.
pub fn logs(gh: &Path, dir: &Path, repo: &str, failed: &[FailedCheck], reg: Reg) -> Vec<Option<String>> {
    ci_logs(gh, dir, repo, failed, reg)
}

/// The fix-main session's brief for main's failing CI, with each log's end where it could be read (`mainCiBrief`).
/// Empty when nothing failed.
pub fn brief(sha: &str, failed: &[FailedCheck], logs: &[Option<String>]) -> String {
    if failed.is_empty() {
        return String::new();
    }
    let each: Vec<String> = failed
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let log = match logs.get(i).and_then(Option::as_deref) {
                Some(log) if !log.is_empty() => format!("\n```\n{}\n```", tail(log, 3000)),
                _ => "\n(Its log couldn't be read here: open the link.)".to_string(),
            };
            format!("### {}\n{}{log}", f.name, f.link)
        })
        .collect();
    format!(
        "CI on the code host failed on the base branch's latest commit ({}), so every change after it starts from a red main:\n\n{}\n\nIf it's CI's own setup failing (a runner, a toolchain, a tool that crashed) rather than the code, the fix may be in the CI config: say what you found in the pull request.",
        short(sha),
        each.join("\n\n")
    )
}

fn short(sha: &str) -> String {
    sha.chars().take(7).collect()
}

/// Whether the host can be asked at all: GitHub, with a repo to name.
pub fn reads(project: &crate::config::ProjectConfig) -> bool {
    project.on_github() && !project.github_repo.is_empty()
}

#[cfg(test)]
mod tests;
