//! What a person has answered for an item (docs/specs/agent/05-item-lifecycle.md, the
//! `needs_clarification` row): kept on the item for good rather than in one message to one
//! session. Every implementer session is told about them, and a question asked again goes back
//! to the session with its answer instead of to the person (2026-10-07: github-workflow asked the
//! same three questions three times; each answer reached one session once, and the next asked
//! again). Pure: no store, no DB; `item_lifecycle.rs`'s `reasks` counter is what caps how many
//! times that can happen before the item needs a human after all.

/// One question a person has answered for an item, kept for good.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub question: String,
    pub answer: String,
    pub at: String,
}

/// How many times in a row an item may ask again what was answered before it's left for a
/// person (`decisions.ts`). It's a project `tuning` limit on the server (`shared/tuning.ts`'s
/// `maxReasks`: def 2, min 0, max 10), not a config key; this is that default, as a constant,
/// the way `release_notes/gather.rs`'s `DEFAULT_MAX_PRS` holds `releaseMaxPrs`'s.
pub const MAX_REASKS: u32 = 2;

/// A question's text, loosely: case and run-of-whitespace insensitive, so the same question
/// asked again in slightly different words still matches.
fn same(s: &str) -> String {
    s.to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The decisions after answering `questions` with `answers` at `at`: a question answered again
/// replaces its old answer. A blank answer isn't a decision.
pub fn with_answers(decisions: &[Decision], questions: &[String], answers: &[String], at: &str) -> Vec<Decision> {
    let mut out = decisions.to_vec();
    for (question, answer) in questions.iter().zip(answers.iter()) {
        let answer = answer.trim();
        if answer.is_empty() {
            continue;
        }
        let decision = Decision {
            question: question.clone(),
            answer: answer.to_string(),
            at: at.to_string(),
        };
        match out.iter().position(|d| same(&d.question) == same(question)) {
            Some(i) => out[i] = decision,
            None => out.push(decision),
        }
    }
    out
}

/// The asked `questions` split into those already answered (with the answer) and those still open.
pub fn split_asked(questions: &[String], decisions: &[Decision]) -> (Vec<Decision>, Vec<String>) {
    let mut answered = Vec::new();
    let mut open = Vec::new();
    for question in questions {
        match decisions.iter().find(|d| same(&d.question) == same(question)) {
            Some(d) => answered.push(d.clone()),
            None => open.push(question.clone()),
        }
    }
    (answered, open)
}

/// The decisions as a prompt section: "Already decided by a person", or nothing without any.
pub fn decisions_text(decisions: &[Decision]) -> String {
    if decisions.is_empty() {
        return String::new();
    }
    let lines: Vec<String> = decisions
        .iter()
        .map(|d| format!("- Q: {}\n  A: {}", d.question, d.answer))
        .collect();
    format!(
        "\n\n## Already decided by a person\n\nThese questions were answered for this item. Treat them as decided, act on them, and don't ask them again:\n\n{}",
        lines.join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(s: &str) -> String {
        s.to_string()
    }

    #[test]
    fn answers_are_kept_by_question_and_a_new_answer_replaces_the_old_one() {
        let one = with_answers(
            &[],
            &[q("Squash or merge?"), q("Keep the old way?")],
            &[q("Squash"), q("Yes")],
            "t1",
        );
        assert_eq!(
            one.iter()
                .map(|d| (d.question.as_str(), d.answer.as_str()))
                .collect::<Vec<_>>(),
            vec![("Squash or merge?", "Squash"), ("Keep the old way?", "Yes")]
        );

        let two = with_answers(&one, &[q("squash  or MERGE?")], &[q("Merge")], "t2");
        assert_eq!(
            two.iter().map(|d| d.answer.as_str()).collect::<Vec<_>>(),
            vec!["Merge", "Yes"],
            "the same question, however it's spaced or cased"
        );
    }

    #[test]
    fn a_blank_answer_is_not_a_decision() {
        assert_eq!(with_answers(&[], &[q("A?")], &[q("")], "t").len(), 0);
    }

    #[test]
    fn asked_questions_split_into_those_answered_and_those_still_open() {
        let decisions = with_answers(&[], &[q("Squash or merge?")], &[q("Squash")], "t");
        let (answered, open) = split_asked(&[q("Squash or merge?"), q("Which branch?")], &decisions);
        assert_eq!(
            answered.iter().map(|d| d.answer.as_str()).collect::<Vec<_>>(),
            vec!["Squash"]
        );
        assert_eq!(open, vec!["Which branch?"]);
    }

    #[test]
    fn the_prompt_section_lists_them_and_is_empty_without_any() {
        assert_eq!(decisions_text(&[]), "");
        let decisions = with_answers(&[], &[q("Squash or merge?")], &[q("Squash")], "t");
        let text = decisions_text(&decisions);
        assert!(text.contains("Already decided by a person"));
        assert!(text.contains("Q: Squash or merge?\n  A: Squash"));
    }
}
