//! Running the planned scouts for a slice and rendering their findings (the scout half of
//! `reviewStep` in Retaskable.com's `agent/src/review.ts`): each job's read-only, ephemeral
//! session, run concurrently through the caller's [`crate::engines::runner::run_session`], then
//! [`format_scout`]'s findings text for the stage 3 reviewer (a separate, not-yet-built backlog
//! item) to read.
use std::collections::HashMap;
use std::future::Future;

use anyhow::Result;
use futures_util::future::join_all;

use crate::engines::{EphemeralSessionConfig, SessionResult, SessionSpec};
use crate::review_context::ReviewContext;
use crate::text::clip;

use super::info::{ScoutInfo, scout_info};
use super::model::ScoutResult;
use super::model::scout_result_schema;
use super::plan::ScoutJob;
use super::prompt::build_prompt;

/// How much of a finding's evidence [`format_scout`] quotes (`cut(..., 400)` in `review.ts`).
const EVIDENCE_MAX: usize = 400;

/// What a scout session runs with, besides the job and [`ReviewContext`].
pub struct Scouts<'a> {
    /// `prompts/scout.md`.
    pub template: &'a str,
    /// `Role::Scout`'s engine/model/budget/local/cursor/sandbox bundle; `session.cwd` is a
    /// checkout at the slice's head, where a scout reads its share of the diff.
    pub session: EphemeralSessionConfig<'a>,
}

/// The read-only, ephemeral session for one scout job.
pub fn session_spec(
    w: &Scouts<'_>,
    ctx: &ReviewContext<'_>,
    job: &ScoutJob,
    info: &HashMap<String, ScoutInfo>,
) -> Result<SessionSpec> {
    let entry = scout_info(info, job.scout)?;
    let prompt = build_prompt(w.template, ctx, job, entry)?;
    Ok(w.session.spec(prompt, scout_result_schema()))
}

/// Runs every job's session concurrently (`Promise.all` in `review.ts`) through `run` (the
/// caller's [`crate::engines::runner::run_session`]), pairing each result with its label. A job
/// whose session spec couldn't even be built, or whose `run` call errors, comes back as an `Err`
/// — [`format_scout`] turns that and an `r.ok == false` result into the same "check it yourself"
/// text.
pub async fn run_scouts<F, Fut>(
    jobs: &[ScoutJob],
    w: &Scouts<'_>,
    ctx: &ReviewContext<'_>,
    info: &HashMap<String, ScoutInfo>,
    run: F,
) -> Vec<(String, Result<SessionResult>)>
where
    F: Fn(SessionSpec) -> Fut,
    Fut: Future<Output = Result<SessionResult>>,
{
    let runs = jobs.iter().map(|job| {
        let run = &run;
        async move {
            let label = job.label.clone();
            let result = match session_spec(w, ctx, job, info) {
                Ok(spec) => run(spec).await,
                Err(e) => Err(e),
            };
            (label, result)
        }
    });
    join_all(runs).await
}

/// One scout's findings, or why nobody checked its concern (`formatScout` in `review.ts`).
pub fn format_scout(label: &str, run: &Result<SessionResult>) -> String {
    let r = match run {
        Ok(r) => r,
        Err(e) => {
            return format!(
                "### {label}\n(This scout failed to start ({e:#}), so nobody has checked this concern for these files. Check it yourself before approving.)"
            );
        }
    };
    if !r.ok {
        return format!(
            "### {label}\n(This scout failed with {}, so nobody has checked this concern for these files. Check it yourself before approving.)",
            r.subtype
        );
    }
    let res = r
        .structured
        .clone()
        .and_then(|v| serde_json::from_value::<ScoutResult>(v).ok())
        .unwrap_or(ScoutResult {
            findings: Vec::new(),
            notes: String::new(),
        });
    let lines: Vec<String> = res
        .findings
        .iter()
        .map(|f| {
            format!(
                "- [{}] {}:{} — {}\n  Evidence: {}",
                f.severity,
                f.file,
                f.line,
                f.body,
                clip(&f.evidence.replace('\n', " "), EVIDENCE_MAX)
            )
        })
        .collect();
    let body = if lines.is_empty() {
        "(no findings)".to_string()
    } else {
        lines.join("\n")
    };
    let notes = if res.notes.is_empty() {
        String::new()
    } else {
        format!("\nNotes: {}", res.notes)
    };
    format!("### {label}\n{body}{notes}")
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::json;

    use super::super::info::parse_scout_info;
    use super::super::plan::ScoutKind;
    use super::*;
    use crate::engines::engine::test_result as result;

    const TEMPLATE: &str = "{{itemId}} {{title}} {{slice}} {{specRefs}} {{sliceDescription}} {{range}} {{scope}} {{files}} {{checks}} {{focus}} {{instructions}}";
    const INFO_JSON: &str = r#"{
        "correctness": { "focus": "correctness", "instructions": "look for bugs" },
        "tests": { "focus": "tests", "instructions": "look for missing tests" },
        "spec": { "focus": "spec", "instructions": "check the specs" }
    }"#;

    fn writer(cwd: &'static Path) -> Scouts<'static> {
        Scouts {
            template: TEMPLATE,
            session: EphemeralSessionConfig {
                cwd,
                budget_usd: 0.5,
                model: "haiku".to_string(),
                local: None,
                engine: None,
                cursor: None,
                sandbox: false,
                shell: true,
            },
        }
    }

    fn ctx<'a>() -> ReviewContext<'a> {
        ReviewContext {
            item_id: "my-item",
            title: "My item",
            slice: 1,
            spec_refs: "(none)",
            slice_description: "(none given)",
            range: "origin/main...HEAD",
            scope: "",
            checks: "(no checks configured)",
        }
    }

    fn jobs() -> Vec<ScoutJob> {
        vec![
            ScoutJob {
                scout: ScoutKind::Correctness,
                label: "correctness".to_string(),
                files: vec!["src/x.rs".to_string()],
            },
            ScoutJob {
                scout: ScoutKind::Tests,
                label: "tests".to_string(),
                files: vec!["src/x.rs".to_string()],
            },
        ]
    }

    #[tokio::test]
    async fn run_scouts_runs_every_job_and_pairs_its_label_with_its_result() {
        let cwd = Path::new("repo");
        let info = parse_scout_info(INFO_JSON).unwrap();
        let answer = json!({ "findings": [], "notes": "" });
        let results = run_scouts(&jobs(), &writer(cwd), &ctx(), &info, |spec| {
            let answer = answer.clone();
            async move {
                assert!(spec.readonly);
                Ok(result(true, "success", Some(answer), None))
            }
        })
        .await;
        assert_eq!(
            results.iter().map(|(label, _)| label.as_str()).collect::<Vec<_>>(),
            vec!["correctness", "tests"]
        );
        assert!(results.iter().all(|(_, r)| r.as_ref().unwrap().ok));
    }

    #[tokio::test]
    async fn run_scouts_reports_a_job_whose_info_is_missing_as_an_error() {
        let cwd = Path::new("repo");
        let mut info = parse_scout_info(INFO_JSON).unwrap();
        info.remove("tests");
        let results = run_scouts(&jobs(), &writer(cwd), &ctx(), &info, |_| async {
            Ok(result(
                true,
                "success",
                Some(json!({ "findings": [], "notes": "" })),
                None,
            ))
        })
        .await;
        let (_, tests_result) = &results[1];
        assert!(tests_result.is_err());
    }

    #[test]
    fn format_scout_renders_each_finding_with_its_evidence_clipped() {
        let evidence = "x".repeat(500);
        let r = Ok(result(
            true,
            "success",
            Some(json!({
                "findings": [{
                    "severity": "major",
                    "file": "src/x.rs",
                    "line": 10,
                    "evidence": evidence,
                    "body": "looks wrong",
                }],
                "notes": "otherwise fine",
            })),
            None,
        ));
        let text = format_scout("correctness", &r);
        assert!(text.starts_with("### correctness\n- [major] src/x.rs:10 — looks wrong"));
        assert!(text.contains("…"), "evidence should be clipped: {text}");
        assert!(text.contains("Notes: otherwise fine"));
    }

    #[test]
    fn format_scout_says_no_findings_when_there_are_none() {
        let r = Ok(result(
            true,
            "success",
            Some(json!({ "findings": [], "notes": "" })),
            None,
        ));
        assert_eq!(format_scout("tests", &r), "### tests\n(no findings)");
    }

    #[test]
    fn format_scout_tells_the_reviewer_to_check_it_themselves_when_the_session_failed() {
        let r = Ok(result(false, "error_during_execution", None, None));
        let text = format_scout("spec", &r);
        assert!(text.contains("failed with error_during_execution"));
        assert!(text.contains("Check it yourself before approving."));
    }

    #[test]
    fn format_scout_tells_the_reviewer_to_check_it_themselves_when_the_session_couldnt_start() {
        let r: Result<SessionResult> = Err(anyhow::anyhow!("no claude"));
        let text = format_scout("correctness", &r);
        assert!(text.contains("failed to start (no claude)"));
    }
}
