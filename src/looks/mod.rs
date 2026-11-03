//! A `look` run's pure part (docs/specs/pieces.md sections 3.2 and 3.5): the brief a read-only session is
//! given, the prompt (`prompts/look.md`), the session and the decision it answers with. Modelled on
//! [`crate::triage`]; `orchestrator::look_run` runs it.

use std::future::Future;

use anyhow::{Context as _, Result};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::backlog::runs::Run;
use crate::config::ProjectType;
use crate::engines::{EphemeralSessionConfig, SessionResult, SessionSpec, describe_failure};
use crate::prompt::{project_words, render_prompt};
use crate::triage::{RetaskableBug, retaskable_bug_schema};

/// How many of an item's history notes a plan's brief carries.
const HISTORY_KEPT: usize = 10;

/// What a look is given, built from the run `run_take` returned. Fields assumed (anything missing or of another
/// type is skipped, never an error):
/// - `piece.title`, `piece.what`, `piece.doneWhen`, `piece.branch`, `piece.change`;
/// - `piece.attempts[]`: `kind`, `outcome`, `why`, `computer`, `engine`;
/// - `piece.looks[]`: `decision`, `reason`, `instructions`;
/// - `history[]` on the look run, present only when the look is into an item's plan: strings, or objects with a
///   `note` (else `text`). Its presence is what makes the brief's `kind` `plan` (otherwise `piece`);
/// - `knownBugs[]` on the run: `id`, `title`.
#[derive(Debug, Clone, PartialEq)]
pub struct LookBrief(Value);

impl LookBrief {
    pub fn from_run(run: &Run) -> Self {
        let p = &run.piece;
        let pick = |v: &Value, keys: &[&str]| -> Value {
            let m: Map<String, Value> = keys
                .iter()
                .filter_map(|k| Some((k.to_string(), v.get(*k).filter(|x| !x.is_null())?.clone())))
                .collect();
            Value::Object(m)
        };
        let list = |v: Option<&Value>, keys: &[&str]| -> Vec<Value> {
            v.and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|x| x.is_object())
                .map(|x| pick(x, keys))
                .collect()
        };
        let mut brief = Map::new();
        let history = run.other.get("history").and_then(Value::as_array);
        brief.insert("kind".into(), json!(if history.is_some() { "plan" } else { "piece" }));
        brief.insert("title".into(), json!(p.title));
        brief.insert("what".into(), json!(p.what));
        brief.insert("doneWhen".into(), json!(p.done_when));
        let attempts: Vec<Value> = p
            .attempts
            .iter()
            .filter(|a| a.is_object())
            .map(|a| pick(a, &["kind", "outcome", "why", "computer", "engine"]))
            .collect();
        brief.insert("attempts".into(), json!(attempts));
        if let Some(b) = p.branch.as_deref().filter(|b| !b.is_empty()) {
            brief.insert("branch".into(), json!(b));
        }
        if let Some(c) = p.change.as_ref().filter(|c| !c.is_null()) {
            brief.insert("change".into(), c.clone());
        }
        brief.insert(
            "looks".into(),
            json!(list(p.other.get("looks"), &["decision", "reason", "instructions"])),
        );
        if let Some(history) = history {
            let notes: Vec<String> = history
                .iter()
                .filter_map(|n| match n {
                    Value::String(s) => Some(s.clone()),
                    _ => n
                        .get("note")
                        .or_else(|| n.get("text"))
                        .and_then(Value::as_str)
                        .map(str::to_string),
                })
                .collect();
            let from = notes.len().saturating_sub(HISTORY_KEPT);
            brief.insert("history".into(), json!(&notes[from..]));
        }
        brief.insert(
            "knownBugs".into(),
            json!(list(run.other.get("knownBugs"), &["id", "title"])),
        );
        LookBrief(Value::Object(brief))
    }
}

/// One piece a `split` makes.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SplitPiece {
    pub title: String,
    pub what: String,
    #[serde(rename = "doneWhen")]
    pub done_when: String,
    #[serde(default)]
    pub needs: Vec<String>,
}

/// `decision` (section 3.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LookDecision {
    Again,
    StartOver,
    Split,
    Wait,
    Person,
}

impl LookDecision {
    fn wire(self) -> &'static str {
        match self {
            LookDecision::Again => "again",
            LookDecision::StartOver => "start_over",
            LookDecision::Split => "split",
            LookDecision::Wait => "wait",
            LookDecision::Person => "person",
        }
    }
}

const ENGINES: [&str; 4] = ["claude", "cursor", "pi", "codex"];

/// The structured answer of a look session.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LookResult {
    pub decision: LookDecision,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub instructions: Option<String>,
    #[serde(default)]
    pub pieces: Vec<SplitPiece>,
    #[serde(default)]
    pub engine: Option<String>,
    #[serde(default, rename = "retaskableBug")]
    pub retaskable_bug: Option<RetaskableBug>,
}

impl LookResult {
    /// Why the answer can't be used, if it can't.
    fn problem(&self, plan: bool) -> Option<&'static str> {
        let has = |s: &Option<String>| s.as_deref().is_some_and(|s| !s.trim().is_empty());
        if self.engine.as_deref().is_some_and(|e| !ENGINES.contains(&e)) {
            return Some("named an engine that doesn't exist");
        }
        match self.decision {
            LookDecision::Split if plan && !has(&self.instructions) => {
                Some("chose to split the plan but gave no instructions")
            }
            LookDecision::Split if !plan && self.pieces.is_empty() => Some("chose to split but gave no pieces"),
            LookDecision::Again if !has(&self.instructions) => Some("chose to try again but gave no instructions"),
            _ => None,
        }
    }

    /// The `looked` result `run_done` sends: camelCase, absent optionals left out.
    pub fn to_wire(&self) -> Value {
        let mut m = Map::new();
        m.insert("decision".into(), json!(self.decision.wire()));
        m.insert("reason".into(), json!(self.reason));
        if let Some(i) = self.instructions.as_deref().filter(|i| !i.trim().is_empty()) {
            m.insert("instructions".into(), json!(i));
        }
        if !self.pieces.is_empty() {
            let pieces: Vec<Value> = self
                .pieces
                .iter()
                .map(|p| json!({ "title": p.title, "what": p.what, "doneWhen": p.done_when, "needs": p.needs }))
                .collect();
            m.insert("pieces".into(), json!(pieces));
        }
        if let Some(e) = &self.engine {
            m.insert("engine".into(), json!(e));
        }
        if let Some(b) = &self.retaskable_bug {
            let mut bug = json!({ "title": b.title, "details": b.details });
            if let Some(s) = &b.same_as {
                bug["sameAs"] = json!(s);
            }
            m.insert("retaskableBug".into(), bug);
        }
        Value::Object(m)
    }
}

/// For `SessionSpec.schema`.
pub fn look_result_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "decision": { "enum": ["again", "start_over", "split", "wait", "person"] },
            "reason": { "type": "string" },
            "instructions": { "type": "string" },
            "pieces": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "title": { "type": "string" },
                        "what": { "type": "string" },
                        "doneWhen": { "type": "string" },
                        "needs": { "type": "array", "items": { "type": "string" } }
                    },
                    "required": ["title", "what", "doneWhen", "needs"]
                }
            },
            "engine": { "enum": ENGINES },
            "retaskableBug": retaskable_bug_schema()
        },
        "required": ["decision", "reason"]
    })
}

/// Fills `prompts/look.md` with this project's words and the brief.
pub fn build_prompt(template: &str, project_type: ProjectType, github_repo: &str, brief: &LookBrief) -> Result<String> {
    let context = serde_json::to_string_pretty(&brief.0).context("rendering the look brief as json")?;
    render_prompt(
        template,
        &[
            ("projectKind", project_words(project_type).kind.to_string()),
            ("githubRepo", github_repo.to_string()),
            ("context", context),
        ],
    )
}

/// What a look session runs with.
pub struct Looker<'a> {
    /// `prompts/look.md`.
    pub template: &'a str,
    pub project_type: ProjectType,
    pub github_repo: &'a str,
    /// The `triager` role's engine on the middle tier with the reviewer's budget; `session.cwd` is the base checkout.
    pub session: EphemeralSessionConfig<'a>,
}

/// The read-only look session: nothing to resume, so it's ephemeral.
pub fn session_spec(l: &Looker<'_>, brief: &LookBrief) -> Result<SessionSpec> {
    let prompt = build_prompt(l.template, l.project_type, l.github_repo, brief)?;
    Ok(l.session.spec(prompt, look_result_schema()))
}

/// What [`decide`] came back with: a result, or why there isn't one.
#[derive(Debug, Clone)]
pub struct Decided {
    pub result: Option<LookResult>,
    pub cost_usd: f64,
    /// Set instead of `result` when the session failed or answered with nothing usable.
    pub note: Option<String>,
}

/// Runs the look session (through `run`, the caller's [`crate::engines::runner::run_session`]) and reads back its
/// decision; anything unusable becomes a note, never a result.
pub async fn decide<F, Fut>(l: &Looker<'_>, brief: &LookBrief, run: F) -> Decided
where
    F: FnOnce(SessionSpec) -> Fut,
    Fut: Future<Output = Result<SessionResult>>,
{
    let noted = |cost_usd: f64, note: String| Decided {
        result: None,
        cost_usd,
        note: Some(note),
    };
    let spec = match session_spec(l, brief) {
        Ok(spec) => spec,
        Err(e) => return noted(0.0, format!("couldn't look into it ({e:#})")),
    };
    let r = match run(spec).await {
        Ok(r) => r,
        Err(e) => return noted(0.0, format!("couldn't look into it ({e:#})")),
    };
    let cost_usd = r.cost_usd.unwrap_or(0.0);
    if !r.ok {
        return noted(cost_usd, describe_failure(&r, l.session.budget_usd));
    }
    match r.structured.and_then(|v| serde_json::from_value::<LookResult>(v).ok()) {
        Some(result) => match result.problem(brief.0["kind"] == "plan") {
            Some(p) => noted(cost_usd, format!("didn't answer with a usable decision ({p})")),
            None => Decided {
                result: Some(result),
                cost_usd,
                note: None,
            },
        },
        None => noted(cost_usd, "didn't answer with a usable decision".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::engines::SessionStart;
    use crate::engines::engine::test_result as result;

    fn looker() -> Looker<'static> {
        Looker {
            template: "{{projectKind}} {{githubRepo}}\n{{context}}",
            project_type: ProjectType::Web,
            github_repo: "x/y",
            session: EphemeralSessionConfig {
                cwd: Path::new("base"),
                budget_usd: 1.5,
                model: "sonnet".to_string(),
                local: None,
                engine: None,
                cursor: None,
                sandbox: false,
                shell: true,
            },
        }
    }

    fn run(kind: &str, extra: Value) -> Run {
        let mut v = json!({
            "token": "t", "kind": kind,
            "piece": {
                "title": "Add X", "what": "do x", "doneWhen": "x works",
                "branch": "retaskable/x", "change": { "url": "https://github.com/x/y/pull/1" },
                "attempts": [
                    { "kind": "build", "outcome": "failed", "why": "tests red", "computer": "box-1", "engine": "codex", "extra": 1 },
                    { "kind": "fix", "outcome": "failed" }
                ],
                "looks": [{ "decision": "again", "reason": "r", "instructions": "be small", "junk": true }]
            },
            "knownBugs": [{ "id": "b1", "title": "A bug", "x": 1 }, "nonsense"]
        });
        v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn a_brief_carries_every_field_of_the_run() {
        let b = LookBrief::from_run(&run("look", json!({}))).0;
        assert_eq!(b["kind"], "piece");
        assert_eq!(b["title"], "Add X");
        assert_eq!(b["doneWhen"], "x works");
        assert_eq!(b["branch"], "retaskable/x");
        assert_eq!(b["change"]["url"], "https://github.com/x/y/pull/1");
        assert_eq!(
            b["attempts"][0],
            json!({ "kind": "build", "outcome": "failed", "why": "tests red", "computer": "box-1", "engine": "codex" })
        );
        assert_eq!(b["attempts"][1], json!({ "kind": "fix", "outcome": "failed" }));
        assert_eq!(
            b["looks"][0],
            json!({ "decision": "again", "reason": "r", "instructions": "be small" })
        );
        assert_eq!(b["knownBugs"], json!([{ "id": "b1", "title": "A bug" }]));
        assert!(b.get("history").is_none());
    }

    #[test]
    fn a_plans_brief_keeps_the_last_ten_history_notes() {
        let history: Vec<Value> = (1..=12)
            .map(|i| {
                if i % 2 == 0 {
                    json!(format!("n{i}"))
                } else {
                    json!({ "note": format!("n{i}") })
                }
            })
            .collect();
        let b = LookBrief::from_run(&run("look", json!({ "history": history }))).0;
        assert_eq!(b["kind"], "plan");
        let h = b["history"].as_array().unwrap();
        assert_eq!(h.len(), 10);
        assert_eq!(h[0], "n3");
        assert_eq!(h[9], "n12");
    }

    #[test]
    fn a_sparse_run_builds_a_brief() {
        let r: Run = serde_json::from_value(json!({ "token": "t", "kind": "look", "piece": {} })).unwrap();
        let b = LookBrief::from_run(&r).0;
        assert_eq!(b["attempts"], json!([]));
        assert_eq!(b["knownBugs"], json!([]));
        assert!(b.get("branch").is_none());
        let r: Run = serde_json::from_value(json!({
            "token": "t", "kind": "look", "piece": { "attempts": [3, null], "looks": "x" }, "knownBugs": 5
        }))
        .unwrap();
        assert_eq!(LookBrief::from_run(&r).0["attempts"], json!([]));
    }

    #[test]
    fn the_real_template_fills_with_the_brief_and_states_both_rules() {
        let template =
            std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("prompts/look.md")).unwrap();
        let brief = LookBrief::from_run(&run("look", json!({})));
        let prompt = build_prompt(&template, ProjectType::Web, "x/y", &brief).unwrap();
        assert!(!prompt.contains("{{"));
        assert!(prompt.contains("x/y") && prompt.contains("a web app"));
        assert!(prompt.contains(&serde_json::to_string_pretty(&brief.0).unwrap()));
        assert!(prompt.contains("Don't pick `again` or `start_over` for work that another, unbuilt item owns"));
        assert!(prompt.contains("Don't repeat a decision an earlier look made that then failed the same way"));
        assert!(!prompt.contains("transcriptsDir"));
    }

    #[test]
    fn session_spec_is_a_read_only_ephemeral_session() {
        let spec = session_spec(&looker(), &LookBrief::from_run(&run("look", json!({})))).unwrap();
        assert!(spec.readonly);
        assert_eq!(spec.start, SessionStart::Ephemeral);
        assert_eq!(spec.schema, look_result_schema());
        assert!(spec.prompt.contains("Add X"));
    }

    async fn decided(answer: Option<Value>, ok: bool) -> Decided {
        decided_for(run("look", json!({})), answer, ok).await
    }

    async fn decided_for(run: Run, answer: Option<Value>, ok: bool) -> Decided {
        let brief = LookBrief::from_run(&run);
        decide(&looker(), &brief, |_| async move {
            Ok(result(
                ok,
                if ok { "success" } else { "error_max_budget_usd" },
                answer,
                None,
            ))
        })
        .await
    }

    #[tokio::test]
    async fn each_decision_parses_and_round_trips_through_the_wire() {
        let piece = json!({ "title": "A", "what": "a", "doneWhen": "d", "needs": ["B"] });
        let bug = json!({ "title": "t", "details": "d", "sameAs": "b1" });
        for answer in [
            json!({ "decision": "again", "reason": "r", "instructions": "i", "engine": "claude" }),
            json!({ "decision": "start_over", "reason": "r", "engine": "pi", "retaskableBug": bug }),
            json!({ "decision": "split", "reason": "r", "pieces": [piece] }),
            json!({ "decision": "wait", "reason": "service down", "engine": "cursor" }),
            json!({ "decision": "person", "reason": "needs access" }),
        ] {
            let d = decided(Some(answer.clone()), true).await;
            assert!(d.note.is_none(), "{answer}");
            assert_eq!(d.result.unwrap().to_wire(), answer);
        }
    }

    #[tokio::test]
    async fn a_plan_may_split_with_instructions_alone() {
        let plan = run("look", json!({ "history": ["a note"] }));
        let answer = json!({ "decision": "split", "reason": "r", "instructions": "smaller" });
        let d = decided_for(plan.clone(), Some(answer.clone()), true).await;
        assert_eq!(d.result.unwrap().to_wire()["instructions"], "smaller");
        let none = json!({ "decision": "split", "reason": "r", "pieces": [] });
        assert!(decided_for(plan, Some(none), true).await.result.is_none());
        // A piece's split needs pieces: instructions alone are unusable.
        let d = decided(Some(answer), true).await;
        assert!(d.result.is_none() && d.note.is_some());
    }

    #[tokio::test]
    async fn an_unusable_answer_gives_a_note_and_no_result() {
        for answer in [
            Some(json!({ "decision": "split", "reason": "r" })),
            Some(json!({ "decision": "again", "reason": "r" })),
            Some(json!({ "decision": "again", "reason": "r", "instructions": " " })),
            Some(json!({ "decision": "wait", "reason": "r", "engine": "gemini" })),
            Some(json!({ "decision": "maybe", "reason": "r" })),
            None,
        ] {
            let d = decided(answer.clone(), true).await;
            assert!(d.result.is_none(), "{answer:?}");
            assert!(d.note.is_some());
        }
        let d = decided(None, false).await;
        assert!(d.result.is_none());
        assert_eq!(d.note.as_deref(), Some("hit its $1.5 budget (spent $0.12)"));
        assert_eq!(d.cost_usd, 0.12);
    }
}
