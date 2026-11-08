//! Stages 2 and 3 of review run on a lane (docs/specs/agent/06.3-review.md): plan the scouts from
//! the lane's diff, run them in parallel as read-only sessions, then run the final reviewer
//! read-only with their findings, and return its answer with the PR comment. The brief is a piece
//! in place of an item (see [`crate::review_context::piece_slice_description`]). Not wired into
//! runs yet; the engine runner is injected, as `run_implementer` takes one.
use std::collections::HashMap;
use std::future::Future;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::config::ReviewConfig;
use crate::engines::{Role, SessionResult, SessionSpec};
use crate::implementer::Playtest;
use crate::review_context::ReviewContext;
use crate::reviewer::{self, Reviewer, ReviewerContext, ReviewerResult};
use crate::scouts::{self, ScoutInfo, Scouts};

/// What the final reviewer is told when no scout ran: to read the whole change itself, for every
/// concern the scouts would have covered (`review.ts`).
fn no_scouts_note(diff: &scouts::DiffInfo, range: &str) -> String {
    let files = diff
        .files
        .iter()
        .map(|f| format!("`{}`", f.file))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "(No scouts ran: read the whole change yourself ({} lines), line by line: `git diff {range} -- <file>` for {files}. Look for logic bugs and broken edge cases, missing tests for behaviour that could break unnoticed, differences from the specs, and committed secrets or risky commands.)",
        diff.total_lines
    )
}

/// Everything one review needs besides the lane and the runner. `common` carries the piece's brief
/// (build it with [`crate::review_context::piece_slice_description`]) and the stage 1 `checks`.
pub struct StageInput<'a> {
    pub base_branch: &'a str,
    pub review: ReviewConfig,
    pub common: ReviewContext<'a>,
    pub scouts: Scouts<'a>,
    pub scout_info: &'a HashMap<String, ScoutInfo>,
    pub reviewer: Reviewer<'a>,
    pub playtest: Option<&'a Playtest>,
    /// "(none)" when there are no approved manual versions to list.
    pub manual_approvals: &'a str,
}

/// The reviewer's answer and the comment to post on the change.
pub struct StageReview {
    pub result: ReviewerResult,
    pub comment: String,
}

/// Runs the scouts and the final reviewer on `lane_dir`'s diff against `origin/<base_branch>`.
/// Fails if the diff can't be read or the reviewer session fails; a failed scout is reported in
/// the findings the reviewer reads (and so in its comment) without failing the review.
pub async fn review_lane<F, Fut>(lane_dir: &Path, input: &StageInput<'_>, run: F) -> Result<StageReview>
where
    F: Fn(Role, SessionSpec) -> Fut,
    Fut: Future<Output = Result<SessionResult>>,
{
    let diff = scouts::diff_info(lane_dir, input.base_branch, "")?;
    let jobs = scouts::plan_scouts(
        &diff,
        input.review.max_scout_diff_lines,
        input.review.min_scout_diff_lines,
    );
    let scout_text = if jobs.is_empty() {
        no_scouts_note(&diff, input.common.range)
    } else {
        scouts::run_scouts(&jobs, &input.scouts, &input.common, input.scout_info, |spec| {
            run(Role::Scout, spec)
        })
        .await
        .iter()
        .map(|(label, r)| scouts::format_scout(label, r))
        .collect::<Vec<_>>()
        .join("\n\n")
    };
    let ctx = ReviewerContext {
        common: input.common,
        diff_stat: &diff.stat,
        scouts: &scout_text,
        playtest: input.playtest,
        manual_approvals: input.manual_approvals,
    };
    let r = run(Role::Reviewer, reviewer::session_spec(&input.reviewer, &ctx)?)
        .await
        .context("running the final reviewer")?;
    if !r.ok {
        bail!("the final reviewer failed with {}", r.subtype);
    }
    let result: ReviewerResult =
        serde_json::from_value(r.structured.context("the final reviewer gave no structured answer")?)
            .context("reading the final reviewer's answer")?;
    let comment = reviewer::format_review(input.common.checks, &result);
    Ok(StageReview { result, comment })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;
    use crate::engines::EphemeralSessionConfig;
    use crate::engines::engine::test_result as result;
    use crate::scouts::parse_scout_info;
    use crate::test_support::{git, repo_with_origin, run as run_git};

    const INFO: &str = r#"{
        "correctness": { "focus": "c", "instructions": "i" },
        "tests": { "focus": "t", "instructions": "i" },
        "spec": { "focus": "s", "instructions": "i" }
    }"#;
    const SCOUT_T: &str = "SCOUT {{files}} {{title}} {{sliceDescription}} {{itemId}} {{slice}} {{specRefs}} {{range}} {{scope}} {{checks}} {{focus}} {{instructions}}";
    const REVIEW_T: &str = "REVIEW {{scouts}} {{title}} {{sliceDescription}} {{itemId}} {{slice}} {{specRefs}} {{range}} {{scope}} {{checks}} {{diffStat}} {{playtest}} {{manualApprovals}}";

    fn session(cwd: &Path) -> EphemeralSessionConfig<'_> {
        EphemeralSessionConfig {
            cwd,
            budget_usd: 1.0,
            model: "m".to_string(),
            local: None,
            engine: None,
            cursor: None,
            sandbox: false,
            shell: true,
        }
    }

    fn lane(lines: usize) -> (tempfile::TempDir, std::path::PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_origin(root.path());
        std::fs::write(repo.join("a.rs"), "x\n".repeat(lines)).unwrap();
        run_git(git(&repo).args(["add", "a.rs"]));
        run_git(git(&repo).args(["commit", "--quiet", "-m", "a"]));
        (root, repo)
    }

    /// Runs the review with a fake runner that records every prompt and fails scouts when asked.
    async fn go(lines: usize, scout_fails: bool) -> (Result<StageReview>, Vec<SessionSpec>) {
        let (_root, repo) = lane(lines);
        let info = parse_scout_info(INFO).unwrap();
        let input = StageInput {
            base_branch: "main",
            review: ReviewConfig {
                max_scout_diff_lines: 1000,
                min_scout_diff_lines: 5,
            },
            common: ReviewContext {
                item_id: "it",
                title: "The piece",
                slice: 1,
                spec_refs: "(none)",
                slice_description: "Piece: build it",
                range: "origin/main...HEAD",
                scope: "",
                checks: "✓ test",
            },
            scouts: Scouts {
                template: SCOUT_T,
                session: session(&repo),
            },
            scout_info: &info,
            reviewer: Reviewer {
                template: REVIEW_T,
                session: session(&repo),
            },
            playtest: None,
            manual_approvals: "(none)",
        };
        let seen = Mutex::new(Vec::new());
        let r = review_lane(&repo, &input, |_, spec: SessionSpec| {
            seen.lock().unwrap().push(spec.clone());
            let is_review = spec.prompt.starts_with("REVIEW");
            let fail = scout_fails && !is_review && spec.prompt.contains("SCOUT") && spec.prompt.contains(" c "); // correctness
            async move {
                if is_review {
                    Ok(result(
                        true,
                        "success",
                        Some(json!({"verdict": "approve", "summary": "Looks right."})),
                        None,
                    ))
                } else if fail {
                    Ok(result(false, "error_max_turns", None, None))
                } else {
                    Ok(result(
                        true,
                        "success",
                        Some(json!({"findings": [{"severity": "minor", "file": "a.rs", "line": 1, "body": "b", "evidence": "e"}], "notes": ""})),
                        None,
                    ))
                }
            }
        })
        .await;
        (r, seen.into_inner().unwrap())
    }

    #[tokio::test]
    async fn a_small_diff_runs_no_scouts_and_tells_the_reviewer_to_read_it() {
        let (r, seen) = go(2, false).await;
        let r = r.unwrap();
        assert_eq!(seen.len(), 1);
        let p = &seen[0].prompt;
        assert!(
            p.contains("No scouts ran: read the whole change yourself (2 lines), line by line"),
            "{p}"
        );
        assert!(p.contains("`git diff origin/main...HEAD -- <file>` for `a.rs`"), "{p}");
        assert!(
            p.contains("logic bugs")
                && p.contains("missing tests")
                && p.contains("differences from the specs")
                && p.contains("committed secrets"),
            "{p}"
        );
        assert!(seen[0].prompt.contains("Piece: build it"));
        assert_eq!(r.result.verdict, "approve");
        assert!(r.comment.contains("**Verdict:** approve") && r.comment.contains("Looks right."));
    }

    #[tokio::test]
    async fn a_large_diff_runs_the_scouts_and_passes_their_findings_on() {
        let (r, seen) = go(50, false).await;
        r.unwrap();
        assert_eq!(seen.len(), 4); // correctness, tests, spec, reviewer
        let review = seen.iter().find(|s| s.prompt.starts_with("REVIEW")).unwrap();
        for label in ["### correctness", "### tests", "### spec"] {
            assert!(review.prompt.contains(label), "{}", review.prompt);
        }
        assert!(review.prompt.contains("- [minor] a.rs:1 — b"));
    }

    #[tokio::test]
    async fn a_failed_scout_is_reported_without_failing_the_review() {
        let (r, seen) = go(50, true).await;
        let r = r.unwrap();
        let review = seen.iter().find(|s| s.prompt.starts_with("REVIEW")).unwrap();
        assert!(review.prompt.contains("This scout failed with error_max_turns"));
        assert_eq!(r.result.verdict, "approve");
    }

    #[tokio::test]
    async fn every_session_is_read_only() {
        let (_, seen) = go(50, false).await;
        assert!(!seen.is_empty());
        assert!(seen.iter().all(|s| s.readonly));
    }
}
