//! The `playtest_group` job (pools-on-server.md section 18.1; `playtestGroupJob` in Retaskable.com's `agent/src/playtestGroups.ts`):
//! one read-only planner session over the try-outs the store sent in `args.entries`, answered with `{ groups: [{ title, why, keys }] }`.
//! The store decides when to group and makes the groups fit its queue; this holds no count, gap, signature, timer or lock.
use std::future::Future;

use anyhow::Result;
use serde::Serialize;
use serde_json::Value;

use super::{JobOutcome, JobSpec, Outcome};
use crate::engines::SessionResult;
use crate::engines::SessionSpec;
use crate::playtest_groups::{Planner, PlaytestEntry, PlaytestGroup, plan};

/// The job's answer.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Grouped {
    pub groups: Vec<PlaytestGroup>,
}

/// `args.entries`: the try-outs to group, each as `{ key, itemId, title, what, steps }`; one with no key is left out.
pub fn entries_of(job: &JobSpec) -> Vec<PlaytestEntry> {
    let text = |e: &Value, k: &str| e.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    (job.args.get("entries").and_then(Value::as_array).into_iter().flatten())
        .filter_map(|e| {
            Some(PlaytestEntry {
                key: e.get("key")?.as_str()?.to_string(),
                item_id: text(e, "itemId"),
                title: text(e, "title"),
                what: text(e, "what"),
                steps: (e.get("steps").and_then(Value::as_array).into_iter().flatten())
                    .filter_map(|s| s.as_str().map(str::to_string))
                    .collect(),
            })
        })
        .collect()
}

/// Runs the planner (`run` is the host's session) and answers its groups as it gave them. A session that did not finish is a failure
/// in words (the store asks again after ten minutes); nothing is guessed.
pub async fn playtest_group_job<F, Fut>(job: &JobSpec, planner: &Planner<'_>, run: F) -> JobOutcome
where
    F: FnOnce(SessionSpec) -> Fut,
    Fut: Future<Output = Result<SessionResult>>,
{
    let entries = entries_of(job);
    if entries.is_empty() {
        return Outcome::<Grouped>::failed("The grouping was sent with no try-outs.").into_job();
    }
    let planned = plan(planner, &entries, run).await;
    match planned.groups {
        Some(groups) => Outcome::done(Grouped { groups }).into_job(),
        None => Outcome::<Grouped>::failed(
            planned
                .note
                .unwrap_or_else(|| "The planner didn't finish grouping the try-outs.".into()),
        )
        .into_job(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::json;

    use super::*;
    use crate::config::ProjectType;
    use crate::engines::EphemeralSessionConfig;
    use crate::engines::engine::test_result as result;

    fn spec(entries: Value) -> JobSpec {
        JobSpec::from_json(&json!({ "kind": "playtest_group", "args": { "signature": "a|b", "entries": entries } }))
            .unwrap()
    }

    fn planner() -> Planner<'static> {
        Planner {
            template: "{{projectNoun}} {{count}} {{list}} {{projectParts}}",
            project_type: ProjectType::Web,
            session: EphemeralSessionConfig {
                cwd: Path::new("."),
                budget_usd: 0.5,
                model: "sonnet".into(),
                local: None,
                engine: None,
                cursor: None,
                sandbox: false,
                shell: false,
            },
        }
    }

    fn entries() -> Value {
        json!([
            { "key": "a-1", "itemId": "a", "title": "A", "what": "Look", "steps": ["Open it", "Click"] },
            { "key": "b-2", "itemId": "b", "title": "B", "what": "Try", "steps": [] },
            { "itemId": "x", "title": "no key" },
        ])
    }

    #[test]
    fn the_entries_are_the_jobs_args_and_one_with_no_key_is_left_out() {
        let e = entries_of(&spec(entries()));
        assert_eq!(e.iter().map(|e| e.key.as_str()).collect::<Vec<_>>(), ["a-1", "b-2"]);
        assert_eq!((e[0].item_id.as_str(), e[0].steps.len()), ("a", 2));
    }

    #[tokio::test]
    async fn the_planners_groups_are_answered_as_given_for_the_store_to_fit() {
        let answer = json!({ "groups": [{ "title": "Build", "why": "same place", "keys": ["a-1", "ghost"] }] });
        let out = playtest_group_job(&spec(entries()), &planner(), |s| async move {
            assert!(
                s.prompt.contains("key: a-1") && s.prompt.contains("key: b-2"),
                "{}",
                s.prompt
            );
            Ok(result(true, "success", Some(answer), None))
        })
        .await;
        let Outcome::Done { result, .. } = out else {
            panic!("{out:?}")
        };
        assert_eq!(
            result,
            json!({ "groups": [{ "title": "Build", "why": "same place", "keys": ["a-1", "ghost"] }] }),
            "not cleaned here: the store makes the groups fit its queue"
        );
    }

    #[tokio::test]
    async fn no_entries_or_an_unfinished_planner_is_a_failure_in_words() {
        let run = |_| async { Ok(result(false, "error_max_budget_usd", None, None)) };
        let out = playtest_group_job(&spec(json!([])), &planner(), run).await;
        assert!(
            matches!(out, Outcome::Failed { ref note, .. } if note.contains("no try-outs")),
            "{out:?}"
        );
        let out = playtest_group_job(&spec(entries()), &planner(), run).await;
        assert!(
            matches!(out, Outcome::Failed { ref note, .. } if note.contains("budget")),
            "{out:?}"
        );
    }
}
