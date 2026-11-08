//! A chore piece (docs/specs/agent/06.7-chores-never-idle.md): a `build`/`fix` piece carrying
//! `chore: { kind, recent? }` among `RunPiece::other`. A chore's implementer runs on the chore tier, and a chore
//! never waits on a person: its own run's `questions` and `person_steps` ends become `failed`.
use serde_json::Value;

use crate::backlog::runs::{RunOutcome, RunPiece};
use crate::tiers::{chore_tier, implementer_model, implementer_tier};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChoreKind {
    FixMain,
    Tidy,
    Tests,
    Bugs,
    Specs,
    SyncMain,
    PlaytestFix,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chore {
    pub kind: ChoreKind,
    /// The recent chores of this kind, one line each (`title: outcome`), so the session doesn't repeat them.
    pub recent: Vec<String>,
    /// The failing checks or the staging smoke report a `fix_main` chore starts from.
    pub report: String,
}

/// The piece's chore, or `None` for a roadmap piece or an unknown kind.
pub fn chore_of(piece: &RunPiece) -> Option<Chore> {
    let kind = match piece.other.get("chore")?.get("kind")?.as_str()? {
        "fix_main" => ChoreKind::FixMain,
        "tidy" => ChoreKind::Tidy,
        "tests" => ChoreKind::Tests,
        "bugs" => ChoreKind::Bugs,
        "specs" => ChoreKind::Specs,
        "sync_main" => ChoreKind::SyncMain,
        "playtest_fix" => ChoreKind::PlaytestFix,
        _ => return None,
    };
    let chore = piece.other.get("chore")?;
    // Each recent chore is a line of text, or `{ title, outcome }`.
    let recent = chore
        .get("recent")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|r| match r {
            Value::String(s) => Some(s.trim().to_string()),
            Value::Object(_) => {
                let word = |k: &str| r.get(k).and_then(Value::as_str).unwrap_or("").trim();
                Some(match (word("title"), word("outcome")) {
                    (t, "") => t.to_string(),
                    ("", o) => o.to_string(),
                    (t, o) => format!("{t}: {o}"),
                })
            }
            _ => None,
        })
        .filter(|l| !l.is_empty())
        .collect();
    let report = chore
        .get("report")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    Some(Chore { kind, recent, report })
}

/// The model an implementer runs on: the chore tier for a chore (a playtest fix aside), else the normal tier.
pub fn model_for(chore: Option<&Chore>, normal_tier: &'static str, item: &crate::backlog::Item) -> &'static str {
    match chore {
        Some(c) if c.kind != ChoreKind::PlaytestFix => chore_tier(),
        _ => implementer_model(normal_tier, item),
    }
}

/// The tier a piece's implementer runs on (see [`model_for`]) for the given usage reading.
pub fn tier_for(chore: Option<&Chore>, now: i64, models: &crate::config::ModelsConfig) -> &'static str {
    match chore {
        Some(c) if c.kind != ChoreKind::PlaytestFix => chore_tier(),
        _ => implementer_tier(None, now, models),
    }
}

/// A chore's own run end: `questions` and `person_steps` become `Failed`, the questions or steps kept in the why.
/// A playtest fix, a main sync and every other outcome are left as they are.
pub fn chore_end(kind: ChoreKind, outcome: RunOutcome, result: &Value) -> RunOutcome {
    if matches!(kind, ChoreKind::PlaytestFix | ChoreKind::SyncMain) {
        return outcome;
    }
    let (lead, list) = match outcome {
        RunOutcome::Questions => (
            "The chore asked questions, and a chore never waits on a person",
            "questions",
        ),
        RunOutcome::PersonSteps => (
            "The chore needs steps only a person can do, and a chore never waits",
            "steps",
        ),
        other => return other,
    };
    let items: Vec<String> = result
        .get(list)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|v| match v.get("question").unwrap_or(v) {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .collect();
    RunOutcome::Failed(format!("{lead}: {}", items.join(" | ")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn piece(extra: Value) -> RunPiece {
        let mut p = json!({ "title": "t" });
        p.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        serde_json::from_value(p).unwrap()
    }

    #[test]
    fn chore_of_reads_each_kind() {
        for (name, kind) in [
            ("fix_main", ChoreKind::FixMain),
            ("tidy", ChoreKind::Tidy),
            ("tests", ChoreKind::Tests),
            ("bugs", ChoreKind::Bugs),
            ("specs", ChoreKind::Specs),
            ("sync_main", ChoreKind::SyncMain),
            ("playtest_fix", ChoreKind::PlaytestFix),
        ] {
            let p = piece(json!({ "chore": { "kind": name, "recent": ["x"] } }));
            assert_eq!(chore_of(&p).map(|c| c.kind), Some(kind));
        }
    }

    #[test]
    fn recent_chores_and_the_report_are_read() {
        let p = piece(json!({ "chore": { "kind": "tidy", "report": " r ", "recent": [
            "a: merged", { "title": "b", "outcome": "failed" }, { "title": "c" }, 3, ""
        ] } }));
        let c = chore_of(&p).unwrap();
        assert_eq!(c.recent, ["a: merged", "b: failed", "c"]);
        assert_eq!(c.report, "r");
    }

    #[test]
    fn no_chore_or_an_unknown_kind_is_not_a_chore() {
        assert_eq!(chore_of(&piece(json!({}))), None);
        assert_eq!(chore_of(&piece(json!({ "chore": { "kind": "juggle" } }))), None);
        assert_eq!(chore_of(&piece(json!({ "chore": {} }))), None);
    }

    #[test]
    fn the_tier_is_the_chore_tier_except_for_a_playtest_fix_or_a_roadmap_piece() {
        let models = crate::config::ModelsConfig::default();
        let normal = implementer_tier(None, 0, &models);
        let of = |kind| Chore {
            kind,
            recent: vec![],
            report: String::new(),
        };
        let (tidy, fix) = (of(ChoreKind::Tidy), of(ChoreKind::PlaytestFix));
        assert_eq!(tier_for(Some(&tidy), 0, &models), chore_tier());
        assert_eq!(tier_for(Some(&fix), 0, &models), normal);
        assert_eq!(tier_for(None, 0, &models), normal);
    }

    #[test]
    fn questions_and_steps_become_failed_with_their_text() {
        let q = json!({ "questions": [{ "question": "Which?", "options": [] }, { "question": "Why?" }] });
        let s = json!({ "steps": ["Add the key.", "Restart."] });
        for kind in [
            ChoreKind::Tidy,
            ChoreKind::Tests,
            ChoreKind::Bugs,
            ChoreKind::Specs,
            ChoreKind::FixMain,
        ] {
            let RunOutcome::Failed(why) = chore_end(kind, RunOutcome::Questions, &q) else {
                panic!()
            };
            assert!(why.contains("Which?") && why.contains("Why?"), "{why}");
            let RunOutcome::Failed(why) = chore_end(kind, RunOutcome::PersonSteps, &s) else {
                panic!()
            };
            assert!(why.contains("Add the key.") && why.contains("Restart."), "{why}");
            assert_eq!(chore_end(kind, RunOutcome::PrOpened, &q), RunOutcome::PrOpened);
        }
    }

    #[test]
    fn a_playtest_fix_and_a_main_sync_are_left_alone() {
        for kind in [ChoreKind::PlaytestFix, ChoreKind::SyncMain] {
            assert_eq!(
                chore_end(kind, RunOutcome::Questions, &json!({})),
                RunOutcome::Questions
            );
            assert_eq!(
                chore_end(kind, RunOutcome::PersonSteps, &json!({})),
                RunOutcome::PersonSteps
            );
        }
    }
}
