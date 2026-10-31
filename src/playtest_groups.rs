//! Playtest groups (docs/specs/agent/06.4-playtests.md's "Grouped into sessions"; Retaskable.com's
//! `agent/src/playtestGroups.ts` is the ground truth): once 4 or more after-merge playtests are
//! waiting, a small read-only Sonnet **planner** session sorts them into themed groups of about
//! 3 to 6, by where in the project they happen, ordered to flow in one sitting.
//!
//! [`crate::backlog::Client`] now has the shared queue's own operations (`tryouts_list`,
//! `tryout_add`, `tryout_result`, `tryout_groups`, the [`TRYOUT_GROUPS_LOCK`] lock), and
//! [`signature`]/[`should_group`] are the pure gate `agent/src/playtestGroups.ts`'s `maybeGroup`
//! runs before planning (06.4's "regrouping... at most every 30 minutes"), but nothing yet calls
//! any of it periodically: there's no orchestrator loop (section 7, not ported yet) to poll
//! `tryouts_list`, run [`plan`] on this round's [`PlaytestEntry`]s under the lock, and send its
//! [`PlaytestGroup`]s on with `tryout_groups`. A caller puts the planning half together the same
//! way as [`crate::triage`]: gather the waiting entries, build a [`Planner`] with the project's
//! words and `budget.plannerUsd`, and run [`plan`] with [`crate::engines::runner::run_session`].
use std::collections::HashSet;
use std::future::Future;

use anyhow::Result;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::ProjectType;
use crate::engines::{EphemeralSessionConfig, SessionResult, SessionSpec, describe_failure};
use crate::prompt::{project_words, render_prompt};

/// The store's `chore_lock` name for grouping (shared/backlogTypes.ts's `TRYOUT_GROUPS_LOCK`):
/// grouping runs on one computer at a time, so every planner round takes this lock first and drops
/// it once `tryout_groups` is sent (or the round gave up).
pub const TRYOUT_GROUPS_LOCK: &str = "tryout-groups";

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
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PlaytestGroup {
    pub title: String,
    pub why: String,
    pub keys: Vec<String>,
}

/// How many must be waiting before the first grouping round runs (playtestGroups.ts's
/// `MIN_TO_GROUP`).
pub const MIN_TO_GROUP: usize = 4;

/// At most this often the queue is regrouped once it's changed again (playtestGroups.ts's
/// `REGROUP_EVERY_MS`; 06.4's "at most every 30 minutes").
pub const REGROUP_EVERY_MS: i64 = 30 * 60_000;

/// The grouping last sent, as far as [`should_group`] needs to know about it
/// ([`crate::backlog::TryoutGrouping`]'s own `signature`/`at`, kept separate so this module stays
/// decoupled from the wire type).
#[derive(Debug, Clone, Copy)]
pub struct LastGrouped<'a> {
    pub signature: &'a str,
    pub at_ms: i64,
}

/// `signature` (playtestGroups.ts): every waiting key, sorted and joined with `|`, so a caller can
/// tell whether the queue actually changed since the last grouping rather than re-running the
/// planner session on every tick.
pub fn signature(keys: &[String]) -> String {
    let mut sorted: Vec<&str> = keys.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    sorted.join("|")
}

/// Whether to run another grouping round now (playtestGroups.ts's `maybeGroup`): at least
/// [`MIN_TO_GROUP`] waiting, and either nothing's been sent yet, or the queue's signature changed
/// and [`REGROUP_EVERY_MS`] has passed since the last round. A signature that hasn't changed never
/// triggers a round on its own, no matter how long ago the last one ran.
pub fn should_group(waiting: usize, signature: &str, last: Option<LastGrouped<'_>>, now_ms: i64) -> bool {
    if waiting < MIN_TO_GROUP {
        return false;
    }
    match last {
        None => true,
        Some(last) if last.signature == signature => false,
        Some(last) => now_ms - last.at_ms >= REGROUP_EVERY_MS,
    }
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

/// Every entry in exactly one group (playtestGroups.ts's `cleanGroups`): a key the session didn't
/// give, or already used by an earlier group (this one included), is dropped rather than trusted;
/// a group left with no keys is dropped too, and whatever's left over lands in "Other".
pub fn clean_groups(groups: Vec<PlaytestGroup>, keys: &[String]) -> Vec<PlaytestGroup> {
    let known: HashSet<&str> = keys.iter().map(String::as_str).collect();
    let mut used: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for g in groups {
        let kept: Vec<String> = g
            .keys
            .into_iter()
            .filter(|k| known.contains(k.as_str()) && used.insert(k.clone()))
            .collect();
        if !kept.is_empty() {
            let title = g.title.trim();
            out.push(PlaytestGroup {
                title: if title.is_empty() {
                    "Playtests".to_string()
                } else {
                    title.to_string()
                },
                why: g.why,
                keys: kept,
            });
        }
    }
    let left: Vec<String> = keys.iter().filter(|k| !used.contains(k.as_str())).cloned().collect();
    if !left.is_empty() {
        out.push(PlaytestGroup {
            title: "Other".to_string(),
            why: "Not grouped with the others.".to_string(),
            keys: left,
        });
    }
    out
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

/// What [`plan`] came back with: the cleaned groups, or why there aren't any (playtestGroups.ts's
/// `plan`, which gives up on the round rather than guessing a grouping).
#[derive(Debug, Clone)]
pub struct Planned {
    pub groups: Option<Vec<PlaytestGroup>>,
    pub cost_usd: f64,
    pub note: Option<String>,
}

/// Runs the planner session (through `run`, the caller's [`crate::engines::runner::run_session`])
/// and reads back its grouping, cleaned with [`clean_groups`] against the entries it was given so
/// a stray or repeated key from the model never reaches the store.
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
        Some(result) => {
            let keys: Vec<String> = entries.iter().map(|e| e.key.clone()).collect();
            Planned {
                groups: Some(clean_groups(result.groups, &keys)),
                cost_usd,
                note: None,
            }
        }
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

    fn group(title: &str, keys: &[&str]) -> PlaytestGroup {
        PlaytestGroup {
            title: title.to_string(),
            why: "because".to_string(),
            keys: keys.iter().map(|k| k.to_string()).collect(),
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

    // signature

    #[test]
    fn signature_sorts_the_keys_and_joins_them() {
        assert_eq!(signature(&["b".to_string(), "a".to_string()]), "a|b");
        assert_eq!(signature(&[]), "");
    }

    // should_group

    #[test]
    fn should_group_is_false_under_the_minimum() {
        assert!(!should_group(3, "a|b|c", None, 0));
    }

    #[test]
    fn should_group_is_true_on_the_first_round_once_the_minimum_is_met() {
        assert!(should_group(4, "a|b|c|d", None, 0));
    }

    #[test]
    fn should_group_is_false_when_the_signature_hasnt_changed_no_matter_the_age() {
        let last = LastGrouped {
            signature: "a|b|c|d",
            at_ms: 0,
        };
        assert!(!should_group(4, "a|b|c|d", Some(last), REGROUP_EVERY_MS * 10));
    }

    #[test]
    fn should_group_waits_out_the_regroup_interval_on_a_changed_signature() {
        let last = LastGrouped {
            signature: "a|b|c|d",
            at_ms: 0,
        };
        assert!(
            !should_group(4, "a|b|c|e", Some(last), REGROUP_EVERY_MS - 1),
            "changed, but too soon"
        );
        assert!(
            should_group(4, "a|b|c|e", Some(last), REGROUP_EVERY_MS),
            "changed, and the wait is over"
        );
    }

    // clean_groups

    #[test]
    fn clean_groups_keeps_every_key_in_exactly_one_group() {
        let keys = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let groups = vec![group("Build", &["a", "b"]), group("Other stuff", &["c"])];
        let cleaned = clean_groups(groups, &keys);
        assert_eq!(cleaned.len(), 2);
        assert_eq!(cleaned[0].keys, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(cleaned[1].keys, vec!["c".to_string()]);
    }

    #[test]
    fn clean_groups_drops_unknown_keys_and_repeats() {
        let keys = vec!["a".to_string(), "b".to_string()];
        let groups = vec![group("Build", &["a", "ghost"]), group("Again", &["a", "b"])];
        let cleaned = clean_groups(groups, &keys);
        assert_eq!(cleaned.len(), 2);
        assert_eq!(cleaned[0].keys, vec!["a".to_string()]);
        assert_eq!(cleaned[1].keys, vec!["b".to_string()]);
    }

    #[test]
    fn clean_groups_sends_leftovers_to_other() {
        let keys = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let groups = vec![group("Build", &["a"])];
        let cleaned = clean_groups(groups, &keys);
        assert_eq!(cleaned.len(), 2);
        assert_eq!(cleaned[1].title, "Other");
        assert_eq!(cleaned[1].keys, vec!["b".to_string(), "c".to_string()]);
    }

    #[test]
    fn clean_groups_drops_a_group_left_with_no_keys() {
        let keys = vec!["a".to_string()];
        let groups = vec![group("Empty", &["ghost"]), group("Build", &["a"])];
        let cleaned = clean_groups(groups, &keys);
        assert_eq!(cleaned.len(), 1);
        assert_eq!(cleaned[0].title, "Build");
    }

    #[test]
    fn clean_groups_falls_back_to_playtests_for_a_blank_title() {
        let keys = vec!["a".to_string()];
        let groups = vec![group("   ", &["a"])];
        let cleaned = clean_groups(groups, &keys);
        assert_eq!(cleaned[0].title, "Playtests");
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
    async fn plan_reads_back_the_sessions_cleaned_groups() {
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
