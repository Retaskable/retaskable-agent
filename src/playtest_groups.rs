//! Playtest groups (docs/specs/agent/06.4-playtests.md's "Grouped into sessions"; Retaskable.com's `agent/src/playtestGroups.ts`): when
//! enough after-merge playtests wait to be too many for one sitting, a small read-only Sonnet **planner** session sorts them into
//! themed groups, by where in the project they happen, ordered to flow in one sitting.
//!
//! The store decides when (pools-on-server.md section 18.1): it sends a `playtest_group` job with the entries in its `args`, holds the
//! `tryout-groups` lock for the task and makes the groups it is answered fit the queue as it is then. This runs the planner over
//! those entries ([`plan`], by [`crate::jobs::playtest_group`]) and answers the groups as the session gave them. No count, gap,
//! signature, minute timer or lock is held here.
use std::future::Future;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::config::ProjectType;
use crate::engines::{EphemeralSessionConfig, SessionResult, SessionSpec, describe_failure};
use crate::prompt::{project_words, render_prompt};

/// One after-merge playtest waiting to be grouped (playtestGroups.ts's `Entry`): `key` is what a
/// group's `keys` names it by, the caller's own (not computed here, since nothing yet reads the
/// shared queue these come from).
#[derive(Debug, Clone)]
pub struct PlaytestEntry {
    pub key: String,
    pub item_id: String,
    pub title: String,
    pub what: String,
    pub steps: Vec<String>,
}

/// One themed session the board would show (`PlaytestGroup`): `keys` in the order to play them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaytestGroup {
    pub title: String,
    pub why: String,
    pub keys: Vec<String>,
}

/// For `SessionSpec.schema`.
pub fn groups_result_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "groups": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "title": { "type": "string" },
                        "why": { "type": "string" },
                        "keys": { "type": "array", "items": { "type": "string" } }
                    },
                    "required": ["title", "why", "keys"]
                }
            }
        },
        "required": ["groups"]
    })
}

#[derive(Debug, Clone, Deserialize)]
struct GroupsResult {
    groups: Vec<PlaytestGroup>,
}

/// Fills `prompts/playtest-groups.md`'s template with this project's words and the waiting
/// playtests, one line per entry (playtestGroups.ts's `plan`'s `list`).
pub fn build_prompt(template: &str, project_type: ProjectType, entries: &[PlaytestEntry]) -> Result<String> {
    let words = project_words(project_type);
    let list = entries
        .iter()
        .map(|e| {
            format!(
                "- key: {}\n  item: {} \u{b7} {}\n  check: {}\n  steps: {}",
                e.key,
                e.item_id,
                e.title,
                e.what,
                e.steps.join(" / "),
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    render_prompt(
        template,
        &[
            ("projectNoun", words.noun.to_string()),
            ("projectParts", words.parts.to_string()),
            ("count", entries.len().to_string()),
            ("list", list),
        ],
    )
}

/// What a planner session runs with, besides the waiting playtests.
pub struct Planner<'a> {
    /// `prompts/playtest-groups.md`.
    pub template: &'a str,
    pub project_type: ProjectType,
    /// `Role::Planner`'s engine/model/budget/local/cursor/sandbox bundle; `session.cwd` is a
    /// checkout of the project, though the prompt tells the session it needn't read anything.
    pub session: EphemeralSessionConfig<'a>,
}

/// The read-only, ephemeral planner session for one round of grouping.
pub fn session_spec(p: &Planner<'_>, entries: &[PlaytestEntry]) -> Result<SessionSpec> {
    let prompt = build_prompt(p.template, p.project_type, entries)?;
    Ok(p.session.spec(prompt, groups_result_schema()))
}

/// What [`plan`] came back with: the groups as the session gave them (the store makes them fit its queue), or why there aren't any
/// (the job is failed rather than guessing a grouping).
#[derive(Debug, Clone)]
pub struct Planned {
    pub groups: Option<Vec<PlaytestGroup>>,
    pub cost_usd: f64,
    pub note: Option<String>,
}

/// Runs the planner session (through `run`, the caller's [`crate::engines::runner::run_session`])
/// and reads back its grouping.
pub async fn plan<F, Fut>(p: &Planner<'_>, entries: &[PlaytestEntry], run: F) -> Planned
where
    F: FnOnce(SessionSpec) -> Fut,
    Fut: Future<Output = Result<SessionResult>>,
{
    let spec = match session_spec(p, entries) {
        Ok(spec) => spec,
        Err(e) => {
            return Planned {
                groups: None,
                cost_usd: 0.0,
                note: Some(format!("couldn't group the playtests ({e:#})")),
            };
        }
    };
    let r = match run(spec).await {
        Ok(r) => r,
        Err(e) => {
            return Planned {
                groups: None,
                cost_usd: 0.0,
                note: Some(format!("couldn't group the playtests ({e:#})")),
            };
        }
    };
    let cost_usd = r.cost_usd.unwrap_or(0.0);
    if !r.ok {
        return Planned {
            groups: None,
            cost_usd,
            note: Some(describe_failure(&r, p.session.budget_usd)),
        };
    }
    let result = r
        .structured
        .and_then(|v| serde_json::from_value::<GroupsResult>(v).ok());
    match result {
        Some(result) => Planned {
            groups: Some(result.groups),
            cost_usd,
            note: None,
        },
        None => Planned {
            groups: None,
            cost_usd,
            note: Some("didn't answer with a usable grouping".to_string()),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::engines::engine::test_result as result;

    const TEMPLATE: &str = "{{projectNoun}} {{count}} {{list}} {{projectParts}}";

    fn entry(key: &str) -> PlaytestEntry {
        PlaytestEntry {
            key: key.to_string(),
            item_id: "1".to_string(),
            title: "Thing".to_string(),
            what: "Look at it".to_string(),
            steps: vec!["Open it".to_string()],
        }
    }

    fn planner(cwd: &'static Path) -> Planner<'static> {
        Planner {
            template: TEMPLATE,
            project_type: ProjectType::Web,
            session: EphemeralSessionConfig {
                cwd,
                budget_usd: 0.5,
                model: "sonnet".to_string(),
                local: None,
                engine: None,
                cursor: None,
                sandbox: false,
                shell: false,
            },
        }
    }

    // build_prompt

    #[test]
    fn build_prompt_lists_every_entry_and_fills_every_placeholder() {
        let prompt = build_prompt(TEMPLATE, ProjectType::Web, &[entry("k1"), entry("k2")]).unwrap();
        assert!(!prompt.contains("{{"));
        assert!(prompt.contains("the web app"));
        assert!(prompt.contains("sign-in"));
        assert!(prompt.contains("key: k1"));
        assert!(prompt.contains("key: k2"));
        assert!(prompt.contains("2"));
    }

    #[test]
    fn build_prompt_fills_the_real_playtest_groups_template() {
        let template = std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("prompts")
                .join("playtest-groups.md"),
        )
        .unwrap();
        let prompt = build_prompt(&template, ProjectType::Web, &[entry("k1")]).unwrap();
        assert!(!prompt.contains("{{"));
        assert!(prompt.contains("key: k1"));
    }

    // session_spec

    #[test]
    fn session_spec_is_a_read_only_ephemeral_planner_session_on_the_budget() {
        let spec = session_spec(&planner(Path::new("repo")), &[entry("k1")]).unwrap();
        assert_eq!(spec.model, "sonnet");
        assert_eq!(spec.budget_usd, 0.5);
        assert!(spec.readonly);
        assert!(!spec.shell);
        assert_eq!(spec.schema, groups_result_schema());
        assert!(spec.prompt.contains("k1"));
    }

    // plan

    #[tokio::test]
    async fn plan_reads_back_the_sessions_groups_as_given() {
        let entries = vec![entry("a"), entry("b")];
        let answer = json!({ "groups": [{ "title": "Build", "why": "same place", "keys": ["a", "b"] }] });
        let planned = plan(&planner(Path::new("repo")), &entries, |_| async move {
            Ok(result(true, "success", Some(answer), None))
        })
        .await;
        let groups = planned.groups.unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].keys, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(planned.cost_usd, 0.12);
        assert!(planned.note.is_none());
    }

    #[tokio::test]
    async fn a_failed_session_leaves_a_note_instead_of_groups() {
        let planned = plan(&planner(Path::new("repo")), &[entry("a")], |_| async {
            Ok(result(false, "error_max_budget_usd", None, None))
        })
        .await;
        assert!(planned.groups.is_none());
        assert_eq!(planned.note.as_deref(), Some("hit its $0.5 budget (spent $0.12)"));
    }

    #[tokio::test]
    async fn a_result_with_no_usable_grouping_leaves_a_note() {
        let planned = plan(&planner(Path::new("repo")), &[entry("a")], |_| async {
            Ok(result(true, "success", None, None))
        })
        .await;
        assert!(planned.groups.is_none());
        assert_eq!(planned.note.as_deref(), Some("didn't answer with a usable grouping"));
    }

    #[tokio::test]
    async fn a_run_that_cant_even_start_leaves_a_note() {
        let planned = plan(&planner(Path::new("repo")), &[entry("a")], |_| async {
            anyhow::bail!("no claude")
        })
        .await;
        assert!(planned.groups.is_none());
        assert_eq!(
            planned.note.as_deref(),
            Some("couldn't group the playtests (no claude)")
        );
        assert_eq!(planned.cost_usd, 0.0);
    }
}
