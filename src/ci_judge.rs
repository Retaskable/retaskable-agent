//! The CI watcher's read-only judge session (docs/specs/ci-watch-through-store.md section 3, `ciWatchFix.ts`'s
//! `judgeRed`): a failing pull request's checks go to one session that reads and changes nothing, and it says
//! whether the pull request's own change broke them, main moved under it, or the machine failed. This is only the
//! session and the `did` the store expects back; the store decides what the verdict means and the pass that
//! schedules it is elsewhere. The caller picks the session's settings: the scout tier ([`writer_model`]), `budget.scoutUsd`,
//! the sandbox setting and the checkout from [`judge_cwd`].
use std::future::Future;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::backlog::ci_result::{CiDid, CiVerdict};
use crate::engines::{EphemeralSessionConfig, SessionResult, SessionSpec};
use crate::lanes::base_checkout_path;
use crate::prompt::render_prompt;

pub const TEMPLATE: &str = include_str!("../prompts/ci-judge.md");

/// Where the session reads: the base checkout when it exists, else the repository itself.
pub fn judge_cwd(worktrees_dir: &Path, repo: &Path) -> PathBuf {
    let planner = base_checkout_path(worktrees_dir);
    if planner.exists() { planner } else { repo.to_path_buf() }
}

pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "verdict": { "enum": ["change", "expectation", "not-change"] },
            "why": { "type": "string" }
        },
        "required": ["verdict", "why"]
    })
}

#[derive(Deserialize)]
struct Answer {
    verdict: CiVerdict,
    why: String,
}

/// The read-only session for a failing pull request; `what` is the failed checks' note.
pub fn session_spec(
    pr: u64,
    title: &str,
    branch: &str,
    what: &str,
    cfg: &EphemeralSessionConfig<'_>,
) -> Result<SessionSpec> {
    let vars = [
        ("pr", pr.to_string()),
        ("title", title.to_string()),
        ("branch", branch.to_string()),
        ("what", what.to_string()),
    ];
    let prompt = render_prompt(TEMPLATE, &vars)?;
    Ok(cfg.spec(prompt.trim_end().to_string(), schema()))
}

/// The verdict and its why, or `None` when the session failed or its answer doesn't fit the schema.
pub fn parse(r: &SessionResult) -> Option<(CiVerdict, String)> {
    let a: Answer = serde_json::from_value(r.ok.then(|| r.structured.clone()).flatten()?).ok()?;
    Some((a.verdict, a.why))
}

/// Runs the session through `run` (the caller's [`crate::engines::run_session`]) and reads it. A session that
/// can't even start gave no verdict.
pub async fn judge<F, Fut>(
    pr: u64,
    title: &str,
    branch: &str,
    what: &str,
    cfg: &EphemeralSessionConfig<'_>,
    run: F,
) -> Option<(CiVerdict, String)>
where
    F: FnOnce(SessionSpec) -> Fut,
    Fut: Future<Output = Result<SessionResult>>,
{
    let spec = session_spec(pr, title, branch, what, cfg).ok()?;
    parse(&run(spec).await.ok()?)
}

/// What the store is told: the verdict (null when none), and whether the instruction was report-only.
pub fn did(judged: Option<&(CiVerdict, String)>, report: bool) -> CiDid {
    CiDid::Judge {
        verdict: judged.map(|(v, _)| *v),
        report,
    }
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;

    use super::*;
    use crate::engines::engine::{test_config, test_result};

    async fn run_with(r: Result<SessionResult>) -> (Option<(CiVerdict, String)>, SessionSpec) {
        let seen = std::sync::Mutex::new(None);
        let got = judge(
            12,
            "Fix the thing",
            "feat/x",
            "check `build` failed",
            &test_config(),
            |spec| {
                *seen.lock().unwrap() = Some(spec);
                async { r }
            },
        )
        .await;
        (got, seen.into_inner().unwrap().unwrap())
    }

    fn said(v: Value) -> Result<SessionResult> {
        Ok(test_result(true, "success", Some(v), None))
    }

    #[tokio::test]
    async fn the_prompt_is_the_typescript_agents_words() {
        let (_, spec) = run_with(said(json!({}))).await;
        assert_eq!(
            spec.prompt,
            "Pull request #12 \"Fix the thing\" (branch feat/x) is failing CI. Read only, change nothing.\n\n\
             check `build` failed\n\n\
             Say which it is: \"change\" (a test or journey this pull request broke), \"expectation\" (main changed under it, so merging main in and rerunning is the fix) or \"not-change\" (the machine failed, not the code). Give one sentence of why."
        );
    }

    #[tokio::test]
    async fn the_session_is_read_only_with_the_verdict_schema_and_the_configs_tier_and_budget() {
        let mut cfg = test_config();
        cfg.model = "haiku".into();
        cfg.budget_usd = 0.5;
        cfg.sandbox = true;
        let spec = session_spec(1, "t", "b", "w", &cfg).unwrap();
        assert!(spec.readonly && spec.shell && spec.sandbox);
        assert_eq!((spec.model.as_str(), spec.budget_usd), ("haiku", 0.5));
        assert_eq!(spec.max_turns, None);
        assert_eq!(spec.schema, schema());
        assert_eq!(
            spec.schema["properties"]["verdict"]["enum"],
            json!(["change", "expectation", "not-change"])
        );
        assert_eq!(spec.schema["required"], json!(["verdict", "why"]));
    }

    #[tokio::test]
    async fn a_good_answer_is_the_verdict_and_why() {
        let (got, _) = run_with(said(json!({ "verdict": "not-change", "why": "The runner timed out." }))).await;
        assert_eq!(got, Some((CiVerdict::NotChange, "The runner timed out.".to_string())));
    }

    #[tokio::test]
    async fn an_answer_that_doesnt_fit_is_none() {
        for bad in [
            json!({ "verdict": "maybe", "why": "x" }),
            json!({ "verdict": "change" }),
            json!({ "why": "x" }),
            json!("change"),
        ] {
            assert_eq!(run_with(said(bad)).await.0, None);
        }
        let none = Ok(test_result(true, "success", None, None));
        assert_eq!(run_with(none).await.0, None);
    }

    #[tokio::test]
    async fn a_failed_session_is_none() {
        let failed = Ok(test_result(
            false,
            "error_max_budget_usd",
            Some(json!({ "verdict": "change", "why": "x" })),
            Some("over"),
        ));
        assert_eq!(run_with(failed).await.0, None);
        assert_eq!(run_with(Err(anyhow!("no claude"))).await.0, None);
    }

    #[test]
    fn the_did_carries_the_verdict_or_null_and_the_report_flag() {
        let some = (CiVerdict::Expectation, "main moved".to_string());
        assert_eq!(
            did(Some(&some), false),
            CiDid::Judge {
                verdict: Some(CiVerdict::Expectation),
                report: false
            }
        );
        assert_eq!(
            did(None, true),
            CiDid::Judge {
                verdict: None,
                report: true
            }
        );
        assert_eq!(
            serde_json::to_value(did(None, true)).unwrap(),
            json!({ "kind": "judge", "verdict": null, "report": true })
        );
    }

    #[test]
    fn the_cwd_is_the_base_checkout_when_there_is_one() {
        let dir = std::env::temp_dir().join(format!("judge-cwd-{}", std::process::id()));
        let repo = dir.join("repo");
        assert_eq!(judge_cwd(&dir, &repo), repo);
        std::fs::create_dir_all(base_checkout_path(&dir)).unwrap();
        assert_eq!(judge_cwd(&dir, &repo), base_checkout_path(&dir));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
