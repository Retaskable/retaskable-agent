//! One tap, or no stop at all (docs/specs/one-tap-or-none.md; this repo's docs/specs/agent/06.2-implementer.md "One tap, or
//! none"): what a session's questions must look like before a person sees them, and when the agent answers them itself.
//! Pure, so each rule has a test.
//!
//! - **Every question has options.** 2 to 4, the recommended first. A result whose question has fewer is sent back to its
//!   session once ([`SEND_BACK`]); if it still has none, the question is asked with the one option [`FOLLOW_THE_BRIEF`]
//!   ([`fill_options`]), so a person is never shown a blank box.
//! - **Obvious questions answer themselves.** The asker marks a question `obvious` (with a `why`) when it would stand behind
//!   the first option with no one asked. When every open question is obvious, has 2 or more options and repeats none already
//!   asked, the first options are the answers ([`answers`]), recorded as "Go with the recommended" records them.
use crate::decisions::Decision;
use crate::implementer::Question;

/// What a session is told when a question had no options (one-tap-or-none.md, piece 5).
pub const SEND_BACK: &str = "Every question needs 2–4 options with the recommended first; if the brief or spec already answers it, follow that and don't ask.";

/// The one option a question still without options is asked with, marked recommended by being first.
pub const FOLLOW_THE_BRIEF: &str = "Follow the brief";

/// The most options a question keeps.
const MAX_OPTIONS: usize = 4;

fn options_of(q: &Question) -> Vec<String> {
    q.options
        .iter()
        .map(|o| o.trim().to_string())
        .filter(|o| !o.is_empty())
        .collect()
}

/// Whether any of the questions has fewer than 2 options (blank ones don't count): it goes back to its session once.
pub fn lacks_options(questions: &[Question]) -> bool {
    questions.iter().any(|q| options_of(q).len() < 2)
}

/// The questions as a person is shown them: options trimmed and cut to 4, and a question with none given the one option
/// [`FOLLOW_THE_BRIEF`]. A question with one option keeps it. Call it after the send-back, never before.
pub fn fill_options(questions: &mut [Question]) {
    for q in questions {
        let mut options = options_of(q);
        options.truncate(MAX_OPTIONS);
        if options.is_empty() {
            options.push(FOLLOW_THE_BRIEF.to_string());
        }
        q.options = options;
    }
}

/// A question's text, loosely: case and whitespace insensitive, so the same question in slightly different words matches.
fn same(s: &str) -> String {
    crate::text::squish(&s.to_lowercase())
}

/// The questions already asked of this piece: the decisions' and those in the store's `Questions: ...\nAnswers: ...` text
/// that `piece_answer` puts in the next run's `next.what`. One per line.
pub fn prior_asked(decisions: &[Decision], next_what: Option<&str>) -> Vec<String> {
    let mut asked: Vec<String> = decisions.iter().map(|d| d.question.clone()).collect();
    if let Some(what) = next_what.and_then(|w| w.strip_prefix("Questions:")) {
        let block = what.split("\nAnswers:").next().unwrap_or(what);
        asked.extend(block.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from));
    }
    asked
}

/// The first options as the answers when every question is `obvious`, has 2 or more options and none repeats one in
/// `asked`; else `None` (one-tap-or-none.md section 2). Never for an empty set.
pub fn answers(questions: &[Question], asked: &[String]) -> Option<Vec<String>> {
    if questions.is_empty() {
        return None;
    }
    let all = questions.iter().all(|q| q.obvious && options_of(q).len() >= 2);
    let repeats = questions
        .iter()
        .any(|q| asked.iter().any(|a| same(a) == same(&q.question)));
    (all && !repeats).then(|| {
        questions
            .iter()
            .map(|q| crate::text::squish(&options_of(q)[0]))
            .collect()
    })
}

/// "Claude went with <answer>: <why>" (the timeline's words, one-tap-or-none.md section 3 step 4).
pub fn went_with(question: &Question, answer: &str) -> String {
    match question.why.trim() {
        "" => format!("Claude went with {answer}"),
        why => format!("Claude went with {answer}: {why}"),
    }
}

/// The answers as `piece_answer` takes them: one per question, a line each, as the app's **Go with the recommended** joins them.
pub fn answers_text(answers: &[String]) -> String {
    answers.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(question: &str, options: &[&str], obvious: bool) -> Question {
        Question {
            question: question.into(),
            options: options.iter().map(|o| o.to_string()).collect(),
            obvious,
            why: "the spec names it".into(),
            ..Default::default()
        }
    }

    #[test]
    fn an_answer_stays_one_line_so_answers_line_up_with_questions() {
        let qs = [q("A?", &["foo\nbar", "b"], true), q("B?", &["c", "d"], true)];
        assert_eq!(answers(&qs, &[]), Some(vec!["foo bar".to_string(), "c".to_string()]));
    }

    #[test]
    fn a_question_with_fewer_than_two_options_lacks_them() {
        assert!(lacks_options(&[q("A?", &[], false)]));
        assert!(lacks_options(&[
            q("A?", &["a", "b"], false),
            q("B?", &["only", " "], false)
        ]));
        assert!(!lacks_options(&[q("A?", &["a", "b"], false)]));
        assert!(!lacks_options(&[]));
    }

    #[test]
    fn a_question_still_without_options_is_asked_with_follow_the_brief() {
        let mut qs = [
            q("A?", &[], false),
            q("B?", &["a", "b", "c", "d", "e"], false),
            q("C?", &["x"], false),
        ];
        fill_options(&mut qs);
        assert_eq!(qs[0].options, [FOLLOW_THE_BRIEF]);
        assert_eq!(qs[1].options, ["a", "b", "c", "d"]);
        assert_eq!(qs[2].options, ["x"]);
    }

    #[test]
    fn an_all_obvious_set_answers_with_its_first_options() {
        let qs = [q("A?", &["first", "second"], true), q("B?", &["yes", "no"], true)];
        assert_eq!(answers(&qs, &[]), Some(vec!["first".into(), "yes".into()]));
        assert_eq!(answers_text(&["first".into(), "yes".into()]), "first\nyes");
    }

    #[test]
    fn a_mixed_set_an_empty_one_and_a_one_option_question_are_asked() {
        assert_eq!(
            answers(&[q("A?", &["a", "b"], true), q("B?", &["a", "b"], false)], &[]),
            None
        );
        assert_eq!(answers(&[], &[]), None);
        assert_eq!(answers(&[q("A?", &["only"], true)], &[]), None);
        assert_eq!(answers(&[q("A?", &[], true)], &[]), None);
    }

    #[test]
    fn a_repeat_of_a_question_already_asked_is_asked_again_to_a_person() {
        let qs = [q("Which  name?", &["a", "b"], true)];
        assert_eq!(answers(&qs, &["which name?".into()]), None);
        assert_eq!(answers(&qs, &["Another?".into()]), Some(vec!["a".into()]));
    }

    #[test]
    fn what_was_asked_before_is_read_from_the_decisions_and_the_stores_text() {
        let d = Decision {
            question: "Squash?".into(),
            answer: "Yes".into(),
            at: String::new(),
        };
        let asked = prior_asked(&[d], Some("Questions: Which name?\nWhich colour?\nAnswers: foo\nred"));
        assert_eq!(asked, ["Squash?", "Which name?", "Which colour?"]);
        assert_eq!(prior_asked(&[], Some("Fix the test")), Vec::<String>::new());
        assert_eq!(prior_asked(&[], None), Vec::<String>::new());
    }

    #[test]
    fn the_timeline_says_what_claude_went_with_and_why() {
        let one = q("A?", &["a", "b"], true);
        assert_eq!(went_with(&one, "a"), "Claude went with a: the spec names it");
        let bare = Question { why: " ".into(), ..one };
        assert_eq!(went_with(&bare, "a"), "Claude went with a");
    }
}
