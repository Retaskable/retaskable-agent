//! One `ask` command, start to finish: checked and acked (or nacked with the reason) at once, then sorted and carried to
//! where it goes, with the outcome reported to the hub as a `chat_report`. A Chat Agent takes no other command.
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use serde_json::Value;

use super::answer::{self, Project, exchanges};
use super::board::{self, Board, Carry, Finished};
use super::classify::{Filing, Reason, Route, route_ask, unavailable_text};
use super::claude::Runner;
use super::report::Ask;
use super::{ONLY_ASKS, ack, nack};
use crate::text::truncate;

/// A command is good for a minute after it is issued, and its id is remembered for ten (cloud.md section 6.2).
const COMMAND_TTL_MS: i64 = 60_000;
const REPLAY_MS: i64 = 10 * 60_000;

/// Command ids seen in the last [`REPLAY_MS`]: a replayed command runs once.
#[derive(Default)]
pub struct Seen(HashMap<String, i64>);

impl Seen {
    fn check(&mut self, id: &str, now: i64) -> bool {
        self.0.retain(|_, at| now - *at <= REPLAY_MS);
        self.0.insert(id.to_string(), now).is_some()
    }
}

/// A `command` message from the hub (cloud.md section 5.3), read.
#[derive(Debug, Clone, PartialEq)]
pub struct Command {
    pub id: String,
    pub name: String,
    pub args: Value,
    pub issued_by: String,
    pub issued_at: i64,
    pub expires_at: i64,
}

impl Command {
    /// `None` when it is not a well-formed command (no id): nothing to answer.
    pub fn parse(msg: &Value) -> Option<Command> {
        let id = msg
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty() && id.len() <= 200)?;
        let text = |k: &str| msg.get(k).and_then(Value::as_str).map(|s| truncate(s, 100));
        Some(Command {
            id: id.to_string(),
            name: text("name").unwrap_or_default(),
            args: msg.get("args").cloned().unwrap_or(Value::Null),
            issued_by: text("issuedBy").unwrap_or_default(),
            issued_at: msg.get("issuedAt").and_then(Value::as_i64).unwrap_or(0),
            expires_at: msg.get("expiresAt").and_then(Value::as_i64).unwrap_or(0),
        })
    }
}

/// What the hub's command comes to: an ask to carry out (and ack), or the reply to send back now.
pub enum Intake {
    Take(Box<Ask>),
    Refuse(Value),
}

/// Checks a command in order: replay, expiry, that it is an `ask` with something in it, and that Claude is here to
/// sort it. A refusal is a `nack` with its reason; nothing else is sent for it.
pub fn intake<R: Runner>(cmd: &Command, seen: &mut Seen, runner: &R, host: &str, now: i64, at: String) -> Intake {
    let refuse = |reason: &str| Intake::Refuse(nack(&cmd.id, reason));
    if seen.check(&cmd.id, now) {
        return refuse("Already received: a command runs once.");
    }
    if now > cmd.expires_at.min(cmd.issued_at.saturating_add(COMMAND_TTL_MS)) {
        return refuse("Expired: it was sent more than 60 s ago (or this machine's clock is off).");
    }
    if cmd.name != "ask" {
        return refuse(ONLY_ASKS);
    }
    let question = cmd
        .args
        .get("question")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    if question.is_empty() {
        return refuse("Ask something first: the message was empty.");
    }
    if !runner.available() {
        return refuse(&unavailable_text(Reason::NoClaude, host));
    }
    let nonempty = |k: &str| {
        cmd.args
            .get(k)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let reply_to = nonempty("replyTo");
    let picture_ids: Vec<String> = cmd
        .args
        .get("pictureIds")
        .and_then(Value::as_array)
        .map(|p| {
            p.iter()
                .filter_map(Value::as_str)
                .filter(|id| board::picture_id(id))
                .take(board::MAX_PICTURES)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    // `audience`: an id, or null for nobody; anything else is left to the project's guess.
    let audience = match cmd.args.get("audience") {
        Some(Value::Null) => Some(None),
        Some(Value::String(a)) if !a.is_empty() => Some(Some(a.clone())),
        _ => None,
    };
    let by = Some(cmd.issued_by.clone()).filter(|b| !b.trim().is_empty() && b != "?");
    Intake::Take(Box::new(Ask {
        id: cmd.id.clone(),
        question: question.to_string(),
        by,
        context: cmd.args.get("context").map(exchanges).unwrap_or_default(),
        plain: reply_to.is_none(),
        reply_to,
        about: cmd.args.get("about").filter(|a| a.is_object()).cloned(),
        picture_ids,
        audience,
        at,
    }))
}

/// Everything an ask needs to be carried out.
pub struct Deps<'a, B, R> {
    pub board: &'a B,
    pub runner: &'a R,
    pub project: &'a Project,
    pub cwd: &'a Path,
    pub host: &'a str,
    pub classify_timeout: Duration,
    pub code_wait: Duration,
    pub code_poll: Duration,
}

/// Sorts the ask and carries it to where it goes. Whatever goes wrong comes back as a `failed` with its reason: nothing
/// is filed or answered on a guess.
pub async fn process<B: Board, R: Runner>(d: &Deps<'_, B, R>, ask: &Ask) -> Finished {
    // Neither the classifier nor the answer has tools to see a picture: they are told how many came, and the verdict
    // still routes the message. The pictures themselves ride with the filed idea and the code question, by store id.
    let seen = with_pictures_note(&ask.question, ask.picture_ids.len());
    let route = route_ask(d.runner, &seen, ask.plain, d.cwd, d.classify_timeout).await;
    match route {
        Route::Unavailable(reason) => Finished::failed(unavailable_text(reason, d.host)),
        Route::Question => answer::answer(d.board, d.runner, d.project, &seen, &ask.context, d.cwd, d.host).await,
        Route::File { kind, title } => match &ask.by {
            Some(login) => {
                let bug = kind == Filing::Bug;
                let carry = Carry {
                    picture_ids: ask.picture_ids.clone(),
                    audience: ask.audience.clone(),
                    about: ask.about.clone(),
                };
                board::file_idea(d.board, &ask.id, &ask.question, title.as_deref(), bug, login, &carry).await
            }
            None => nobody(),
        },
        Route::Code => match &ask.by {
            Some(login) => {
                let (id, q) = (&ask.id, &ask.question);
                board::ask_coding_agent(d.board, id, q, login, &ask.picture_ids, d.code_wait, d.code_poll).await
            }
            None => nobody(),
        },
    }
}

/// The message with a line saying how many pictures came with it, for a session that cannot see them.
fn with_pictures_note(text: &str, n: usize) -> String {
    match n {
        0 => text.to_string(),
        n => format!("{text}\n\n(The message came with {n} picture(s) you cannot see.)"),
    }
}

fn nobody() -> Finished {
    Finished::failed("The Chat Agent doesn't know who sent this, so it filed and asked nothing. Send it again.")
}

/// The ack for an ask that was taken.
pub fn taken(ask: &Ask) -> Value {
    ack(&ask.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::board::fake::Fake as FakeBoard;
    use crate::chat::claude::fake::Fake;
    use crate::engines::engine::test_result;
    use serde_json::json;

    const NOW: i64 = 1_000_000;

    fn msg(name: &str, args: Value) -> Command {
        Command::parse(&json!({
            "type": "command", "id": "c1", "name": name, "args": args,
            "issuedBy": "ada", "issuedAt": NOW - 1000, "expiresAt": NOW + 50_000
        }))
        .unwrap()
    }

    fn take(cmd: &Command, seen: &mut Seen, runner: &Fake) -> Intake {
        intake(cmd, seen, runner, "box", NOW, "2026-11-13T00:00:00Z".into())
    }

    fn refusal(i: Intake) -> String {
        match i {
            Intake::Refuse(v) => {
                assert_eq!(v["type"], "nack");
                v["reason"].as_str().unwrap().to_string()
            }
            Intake::Take(_) => panic!("taken"),
        }
    }

    #[test]
    fn only_an_ask_is_taken_and_every_other_command_is_nacked() {
        let runner = Fake::new(vec![]);
        for name in ["pause", "draft_spec", "idea_accept", "play", "restart"] {
            let r = refusal(take(&msg(name, json!({})), &mut Seen::default(), &runner));
            assert_eq!(r, ONLY_ASKS);
        }
        let Intake::Take(a) = take(
            &msg("ask", json!({ "question": " hi " })),
            &mut Seen::default(),
            &runner,
        ) else {
            panic!("refused")
        };
        assert_eq!(
            (a.id.as_str(), a.question.as_str(), a.by.as_deref(), a.plain),
            ("c1", "hi", Some("ada"), true)
        );
        assert_eq!(taken(&a), json!({ "type": "ack", "id": "c1" }));
    }

    #[test]
    fn a_replay_an_expired_command_and_an_empty_message_are_nacked() {
        let runner = Fake::new(vec![]);
        let mut seen = Seen::default();
        let cmd = msg("ask", json!({ "question": "hi" }));
        assert!(matches!(take(&cmd, &mut seen, &runner), Intake::Take(_)));
        assert!(refusal(take(&cmd, &mut seen, &runner)).starts_with("Already received"));
        let old = Command {
            id: "c2".into(),
            issued_at: NOW - 61_000,
            ..cmd.clone()
        };
        assert!(refusal(take(&old, &mut seen, &runner)).starts_with("Expired"));
        let empty = msg("ask", json!({ "question": "  " }));
        let empty = Command {
            id: "c3".into(),
            ..empty
        };
        assert!(refusal(take(&empty, &mut seen, &runner)).starts_with("Ask something"));
    }

    #[test]
    fn no_claude_is_a_nack_with_the_reason() {
        let mut runner = Fake::new(vec![]);
        runner.available = false;
        let r = refusal(take(
            &msg("ask", json!({ "question": "hi" })),
            &mut Seen::default(),
            &runner,
        ));
        assert_eq!(r, unavailable_text(Reason::NoClaude, "box"));
    }

    #[test]
    fn a_reply_or_pictures_are_not_plain_and_the_context_comes_along() {
        let runner = Fake::new(vec![]);
        let args = json!({ "question": "and?", "replyTo": "c0", "context": [{ "question": "a", "answer": "b", "outcomes": [] }] });
        let Intake::Take(a) = take(&msg("ask", args), &mut Seen::default(), &runner) else {
            panic!()
        };
        assert!(!a.plain && a.reply_to.as_deref() == Some("c0") && a.context.len() == 1);
    }

    const PIC: &str = "01234567-89ab-cdef-0123-456789abcdef.png";

    #[test]
    fn pictures_stay_plain_and_only_valid_ids_come_along_with_the_audience() {
        let runner = Fake::new(vec![]);
        let args = json!({ "question": "see", "pictureIds": [PIC, "p1", "../x", 5], "audience": null });
        let Intake::Take(a) = take(&msg("ask", args), &mut Seen::default(), &runner) else {
            panic!()
        };
        assert!(a.plain);
        assert_eq!((a.picture_ids, a.audience), (vec![PIC.to_string()], Some(None)));
        let args = json!({ "question": "x", "audience": "owners" });
        let Intake::Take(a) = take(&msg("ask", args), &mut Seen::default(), &runner) else {
            panic!()
        };
        assert_eq!(a.audience, Some(Some("owners".to_string())));
    }

    fn ask(by: Option<&str>) -> Ask {
        Ask {
            id: "c1".into(),
            question: "it crashes".into(),
            by: by.map(str::to_string),
            context: vec![],
            plain: true,
            reply_to: None,
            about: None,
            picture_ids: vec![],
            audience: None,
            at: String::new(),
        }
    }

    async fn run(runner: &Fake, board: &FakeBoard, ask: &Ask) -> Finished {
        let project = Project::default();
        let d = Deps {
            board,
            runner,
            project: &project,
            cwd: Path::new("."),
            host: "box",
            classify_timeout: crate::chat::classify::TIMEOUT,
            code_wait: Duration::from_secs(10),
            code_poll: Duration::from_secs(1),
        };
        process(&d, ask).await
    }

    fn sorted(v: Value) -> Fake {
        Fake::new(vec![Ok(test_result(true, "success", Some(v), None))])
    }

    #[tokio::test]
    async fn a_confident_bug_is_filed_for_the_asker() {
        let runner = sorted(json!({ "kind": "bug", "confidence": 0.9, "title": "Crash" }));
        let board = FakeBoard::new(|_, _, _| Ok(json!({ "id": "idea-1" })));
        let r = run(&runner, &board, &ask(Some("ada"))).await;
        assert_eq!(
            (r.done, r.answer.as_str(), r.idea_id.as_deref()),
            (true, "Filed as a bug: Crash", Some("idea-1"))
        );
        assert_eq!(board.ops(), ["idea_add"]);
    }

    #[tokio::test]
    async fn a_message_with_pictures_is_classified_and_filed_with_them_and_its_card() {
        let runner = sorted(json!({ "kind": "bug", "confidence": 0.9, "title": "Crash" }));
        let board = FakeBoard::new(|_, _, _| Ok(json!({ "id": "idea-1" })));
        let mut a = ask(Some("ada"));
        a.picture_ids = vec![PIC.to_string()];
        a.about = Some(json!({ "kind": "item", "id": "i9" }));
        a.audience = Some(Some("owners".into()));
        let r = run(&runner, &board, &a).await;
        assert!(r.done);
        let specs = runner.specs.lock().unwrap();
        assert_eq!(specs.len(), 1, "the classifier ran");
        assert!(specs[0].prompt.contains("1 picture(s)"));
        let sent = board.sent.lock().unwrap();
        let args = &sent[0].1;
        assert_eq!(
            (
                args["pictureIds"].clone(),
                args["audience"].clone(),
                args["revises"].clone()
            ),
            (json!([PIC]), json!("owners"), json!("i9"))
        );
        assert_eq!(args["about"], json!({ "kind": "item", "id": "i9" }));
        assert_eq!(
            args["experience"], "it crashes",
            "the idea keeps the person's exact words"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_code_question_with_pictures_posts_their_ids() {
        let runner = sorted(json!({ "kind": "code", "confidence": 0.9 }));
        let board = FakeBoard::new(|op, _, _| match op {
            "intent_request" => Ok(json!({ "id": "r1" })),
            _ => Ok(json!([{ "id": "r1", "status": "done", "result": { "answer": "In x.rs" } }])),
        });
        let mut a = ask(Some("ada"));
        a.picture_ids = vec![PIC.to_string()];
        assert!(run(&runner, &board, &a).await.done);
        assert_eq!(board.sent.lock().unwrap()[0].1["args"]["pictureIds"], json!([PIC]));
    }

    #[tokio::test(start_paused = true)]
    async fn a_code_question_is_posted_and_its_answer_returned() {
        let runner = sorted(json!({ "kind": "code", "confidence": 0.9 }));
        let board = FakeBoard::new(|op, _, _| match op {
            "intent_request" => Ok(json!({ "id": "r1" })),
            _ => Ok(json!([{ "id": "r1", "status": "done", "result": { "answer": "In x.rs" } }])),
        });
        let r = run(&runner, &board, &ask(Some("ada"))).await;
        assert_eq!(r, Finished::done("In x.rs"));
    }

    #[tokio::test]
    async fn a_question_is_answered_and_files_nothing() {
        let runner = Fake::new(vec![
            Ok(test_result(
                true,
                "success",
                Some(json!({ "kind": "question", "confidence": 0.9 })),
                None,
            )),
            Ok(test_result(
                true,
                "success",
                Some(json!({ "answer": "Two things.", "actions": [] })),
                None,
            )),
        ]);
        let board = FakeBoard::new(|_, _, _| Ok(json!([])));
        let r = run(&runner, &board, &ask(Some("ada"))).await;
        assert_eq!(r, Finished::done("Two things."));
        assert!(board.ops().iter().all(|o| o != "idea_add" && o != "intent_request"));
    }

    #[tokio::test]
    async fn a_classifier_that_cannot_run_files_and_answers_nothing() {
        let signed_out = crate::engines::SessionResult {
            signed_out: true,
            ..test_result(false, "error", None, Some("x"))
        };
        let runner = Fake::new(vec![Ok(signed_out)]);
        let board = FakeBoard::new(|_, _, _| Ok(json!({})));
        let r = run(&runner, &board, &ask(Some("ada"))).await;
        assert_eq!(r, Finished::failed(unavailable_text(Reason::SignedOut, "box")));
        assert!(board.ops().is_empty());
        assert_eq!(runner.specs.lock().unwrap().len(), 1, "no second session to answer");
    }

    #[tokio::test]
    async fn an_ask_from_nobody_is_not_filed() {
        let runner = sorted(json!({ "kind": "idea", "confidence": 0.9 }));
        let board = FakeBoard::new(|_, _, _| Ok(json!({})));
        let r = run(&runner, &board, &ask(None)).await;
        assert!(!r.done);
        assert!(board.ops().is_empty());
    }
}
