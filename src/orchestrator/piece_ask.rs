//! What a `build` or `fix` run does with the questions its session asks (docs/specs/agent/06.2-implementer.md "One tap, or
//! none"; docs/specs/one-tap-or-none.md): a question without options goes back to the session once, then is asked with
//! "Follow the brief"; and when every question is obvious the run's end carries the answers, which the loop sends to the store
//! with `piece_answer` once the store has taken the end (`driver/runs/owed.rs`), so the piece carries on with no Needs-you stop.
use serde_json::{Value, json};

use crate::backlog::runs::{RunKind, RunPiece};
use crate::implementer::{ImplementerResult, Question, piece_decision_list};
use crate::obvious;
use crate::runs::chore::chore_of;

/// The key the run's end carries its answers under. The loop takes it off before the end goes to the store.
pub const AUTO_ANSWER: &str = "autoAnswer";

/// The tiers whose stops are always a person's, whatever a question says (one-tap-or-none.md section 2).
const PERSON_TIERS: [&str; 2] = ["needsyou", "tryfirst"];

/// Whether the session asked, and at least one of its questions lacks options: it is sent back once.
pub fn asked_without_options(answer: &anyhow::Result<ImplementerResult>) -> bool {
    matches!(answer, Ok(r) if r.status == "needs_clarification" && obvious::lacks_options(&r.questions))
}

/// A piece that is never answered for: a chore (a playtest fix is one), a fix of a try-out's problem, a piece added to a finished
/// card, one held for a check, and one the store says is in the `needsyou` or `tryfirst` tier or checked by a person.
fn kept_for_a_person(piece: &RunPiece, kind: RunKind) -> bool {
    let text = |key: &str| piece.other.get(key).and_then(Value::as_str);
    let next = piece
        .other
        .get("next")
        .and_then(|n| n.get("what"))
        .and_then(Value::as_str);
    chore_of(piece).is_some()
        || piece.other.contains_key("purpose")
        || piece.held.is_some()
        || text("tier").is_some_and(|t| PERSON_TIERS.contains(&t))
        || text("checking") == Some("manual")
        || (kind == RunKind::Fix && next.is_some_and(|w| w.starts_with("A try-out found a problem")))
}

/// How many times this piece has been asked about already: its earlier runs that ended in questions, or the one that the
/// store's `Questions: ...` text in `next` says was answered.
fn asked_before(piece: &RunPiece) -> usize {
    let ended = piece
        .attempts
        .iter()
        .filter(|a| a.get("outcome").and_then(Value::as_str) == Some("questions"))
        .count();
    let answered = next_what(piece).is_some_and(|w| w.starts_with("Questions:"));
    ended.max(usize::from(answered))
}

fn next_what(piece: &RunPiece) -> Option<&str> {
    piece.other.get("next")?.get("what")?.as_str()
}

/// The piece's name for `piece_answer`: `<item>#<n>`, from its record id `<item>-piece-<n>`.
fn piece_name(piece: &RunPiece) -> Option<String> {
    let n = piece.other.get("n")?.as_u64()?;
    let id = piece.other.get("id")?.as_str()?;
    let item = id.strip_suffix(&format!("-piece-{n}"))?;
    Some(format!("{item}#{n}"))
}

/// The answers this run's end carries, when every question is obvious (`obvious::answers`), the piece may be answered for, and
/// the earlier asks are under the store's `maxReasks` (the store's numbers unknown: a person is asked). `None` asks a person.
pub fn auto_answer(piece: &RunPiece, kind: RunKind, max_reasks: Option<u32>, questions: &[Question]) -> Option<Value> {
    let max = usize::try_from(max_reasks?).ok()?;
    if kept_for_a_person(piece, kind) || asked_before(piece) + 1 > max {
        return None;
    }
    let asked = obvious::prior_asked(&piece_decision_list(piece), next_what(piece));
    let answers = obvious::answers(questions, &asked)?;
    let went: Vec<String> = questions
        .iter()
        .zip(&answers)
        .map(|(q, a)| obvious::went_with(q, a))
        .collect();
    Some(json!({ "piece": piece_name(piece)?, "answers": obvious::answers_text(&answers), "went": went }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn piece(extra: Value) -> RunPiece {
        let mut v = json!({ "title": "T", "what": "W", "doneWhen": "D", "n": 5, "id": "one-tap-or-none-piece-5" });
        v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        serde_json::from_value(v).unwrap()
    }

    fn obvious_q(text: &str) -> Question {
        Question {
            question: text.into(),
            options: vec!["foo".into(), "bar".into()],
            obvious: true,
            why: "the spec names it".into(),
            ..Default::default()
        }
    }

    fn answered(extra: Value, qs: &[Question]) -> Option<Value> {
        auto_answer(&piece(extra), RunKind::Build, Some(2), qs)
    }

    #[test]
    fn an_all_obvious_set_is_answered_for_the_named_piece_with_a_line_each() {
        let got = answered(json!({}), &[obvious_q("Which name?"), obvious_q("Which colour?")]).unwrap();
        assert_eq!(got["piece"], "one-tap-or-none#5");
        assert_eq!(got["answers"], "foo\nfoo");
        assert_eq!(got["went"][0], "Claude went with foo: the spec names it");
    }

    #[test]
    fn a_mixed_set_is_asked() {
        let mut other = obvious_q("Which colour?");
        other.obvious = false;
        assert!(answered(json!({}), &[obvious_q("Which name?"), other]).is_none());
    }

    #[test]
    fn a_repeat_is_asked_whether_the_store_or_the_decisions_remember_it() {
        let q = [obvious_q("Which name?")];
        let next = json!({ "next": { "kind": "fix", "what": "Questions: Which name?\nAnswers: foo" } });
        assert!(auto_answer(&piece(next), RunKind::Fix, Some(3), &q).is_none());
        let decided = json!({ "decisions": [{ "question": "which name?", "answer": "foo" }] });
        assert!(answered(decided, &q).is_none());
    }

    #[test]
    fn the_store_s_reask_limit_holds() {
        let q = [obvious_q("Which name?")];
        let earlier = json!({ "attempts": [{ "kind": "build", "outcome": "questions" }, { "kind": "build", "outcome": "questions" }] });
        assert!(answered(earlier, &q).is_none());
        let one = json!({ "attempts": [{ "kind": "build", "outcome": "questions" }] });
        assert!(answered(one, &q).is_some());
        assert!(
            auto_answer(&piece(json!({})), RunKind::Build, None, &q).is_none(),
            "no numbers from the store: ask"
        );
    }

    #[test]
    fn the_tier_chore_and_playtest_fix_exceptions_hold() {
        let q = [obvious_q("Which name?")];
        for extra in [
            json!({ "tier": "needsyou" }),
            json!({ "tier": "tryfirst" }),
            json!({ "checking": "manual" }),
            json!({ "chore": { "kind": "tidy", "title": "t" } }),
            json!({ "chore": { "kind": "playtest_fix", "title": "t" } }),
            json!({ "held": "touches sign-in" }),
            json!({ "purpose": "remove_flag" }),
        ] {
            assert!(answered(extra.clone(), &q).is_none(), "{extra}");
        }
        let fix = json!({ "next": { "kind": "fix", "what": "A try-out found a problem: it flickers" } });
        assert!(auto_answer(&piece(fix), RunKind::Fix, Some(2), &q).is_none());
        assert!(answered(json!({ "tier": "routine" }), &q).is_some());
    }

    #[test]
    fn a_question_without_options_is_sent_back_only_when_the_session_asked() {
        let mut r: ImplementerResult = serde_json::from_value(json!({
            "status": "needs_clarification", "summary": "s", "questions": [{ "question": "What should Change it do?" }]
        }))
        .unwrap();
        assert!(asked_without_options(&Ok(r.clone())));
        r.questions[0].options = vec!["a".into(), "b".into()];
        assert!(!asked_without_options(&Ok(r.clone())));
        r.questions.clear();
        r.status = "pr_opened".into();
        assert!(!asked_without_options(&Ok(r)));
    }
}
