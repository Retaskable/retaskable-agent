//! The Chat Agent's reach into the shared backlog (chat-agent-process.md section 5). It may read the board and write two
//! things only: `idea_add` and `intent_request`, each for the person who typed the message. The store enforces the
//! list; [`check_op`] keeps it here too, so the agent refuses to send what the store would refuse.
use std::future::Future;
use std::time::Duration;

use serde_json::{Value, json};

use crate::backlog::{BacklogError, Client};
use crate::link::{DeviceTokens, TokenAnswer};

/// How long a code question waits for a Coding Agent's answer (`CODE_QUESTION_WAIT_MS`).
pub const CODE_QUESTION_WAIT: Duration = Duration::from_secs(300);
/// How often the row is looked at (`CODE_POLL_MS`).
pub const CODE_POLL: Duration = Duration::from_secs(2);
/// What the chat says when no Coding Agent answered.
pub const NONE_WAS_FREE: &str = "No Coding Agent was free to answer. Try again, or check the Agents page.";

const READS: [&str; 16] = [
    "list",
    "drafts",
    "ideas_list",
    "facts_get",
    "settings_get",
    "pause_get",
    "intent_list",
    "details_get",
    "draft_details_get",
    "tryouts_list",
    "logs_get",
    "releases_get",
    "deploy_list",
    "rollout_get",
    "needs_status",
    "version_names_get",
];

/// A store the Chat Agent talks to: one op at a time, an error already in words for the chat.
pub trait Board: Send + Sync {
    fn op(&self, op: &'static str, args: Value) -> impl Future<Output = Result<Value, String>> + Send;
}

/// `for: { via: "chat", login }` with a non-empty login: the person the message is for.
fn for_chat(args: &Value) -> bool {
    let f = args.get("for");
    f.and_then(|f| f.get("via")).and_then(Value::as_str) == Some("chat")
        && f.and_then(|f| f.get("login"))
            .and_then(Value::as_str)
            .is_some_and(|l| !l.trim().is_empty())
}

/// The allowlist (`chatOpRefusal`): `Err` says why the op is not the Chat Agent's to send.
pub fn check_op(op: &str, args: &Value) -> Result<(), String> {
    if READS.contains(&op) {
        return Ok(());
    }
    match op {
        "idea_add" if for_chat(args) => Ok(()),
        "idea_add" => Err("idea_add is only for the person who typed the message (for: via chat, with a login)".into()),
        "intent_request" => {
            let nested = args.get("args");
            let ok = for_chat(args)
                && args.get("kind").and_then(Value::as_str) == Some("ask")
                && nested.and_then(|a| a.get("codeQuestion")) == Some(&Value::Bool(true))
                && nested.and_then(|a| a.get("question")).is_some_and(Value::is_string)
                && nested.and_then(|a| a.get("askId")).is_some_and(Value::is_string);
            if ok {
                Ok(())
            } else {
                Err("intent_request is only a code question: kind ask, codeQuestion true, a question and an askId, for the asker".into())
            }
        }
        other => Err(format!("The Chat Agent may not run {other}")),
    }
}

/// The shared store, for one project: this agent's own link, the project's repository id.
pub struct StoreBoard {
    pub client: Client,
    pub tokens: DeviceTokens,
    pub repo_id: u64,
}

impl Board for StoreBoard {
    async fn op(&self, op: &'static str, args: Value) -> Result<Value, String> {
        check_op(op, &args)?;
        let mut stale: Option<String> = None;
        loop {
            let token = match self.tokens.access_token(stale.as_deref()).await {
                TokenAnswer::Token { access_token, .. } => access_token,
                TokenAnswer::Idle => {
                    return Err(
                        "Chat Agent isn't linked to the shared backlog. Run `retaskable-chat-agent link`.".into(),
                    );
                }
                TokenAnswer::Unreachable => return Err("Chat Agent couldn't reach the shared backlog.".into()),
            };
            match self
                .client
                .op::<Value>(self.repo_id, Some(&token), op, args.clone(), "answer")
                .await
            {
                // An access token the store no longer takes: one fresh one, once.
                Err(BacklogError::BadAnswer { status: 401, .. }) if stale.is_none() => stale = Some(token),
                other => return other.map_err(|e| e.to_string()),
            }
        }
    }
}

/// How an ask ended.
#[derive(Debug, Clone, PartialEq)]
pub struct Finished {
    pub done: bool,
    pub answer: String,
    /// The idea this made, when it filed one (Undo bins it).
    pub idea_id: Option<String>,
}

impl Finished {
    pub fn done(answer: impl Into<String>) -> Self {
        Finished {
            done: true,
            answer: answer.into(),
            idea_id: None,
        }
    }
    pub fn failed(answer: impl Into<String>) -> Self {
        Finished {
            done: false,
            answer: answer.into(),
            idea_id: None,
        }
    }
}

/// `clientId`: the same ask filed twice (a restart between the filing and the answer) finds the idea it made.
pub fn client_id(ask_id: &str) -> String {
    let id: String = format!("chat-{ask_id}")
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    id.chars().take(60).collect()
}

/// Files the message as the asker's own bug or idea, planned automatically and asked about first.
pub async fn file_idea<B: Board>(
    board: &B,
    ask_id: &str,
    question: &str,
    title: Option<&str>,
    bug: bool,
    login: &str,
) -> Finished {
    let first_line = question.lines().next().unwrap_or_default().trim();
    let title: String = title
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or(first_line)
        .chars()
        .take(80)
        .collect();
    let args = json!({
        "title": title,
        "experience": question,
        "freeText": true,
        "autoPlan": true,
        "askFirst": true,
        "bug": bug,
        "clientId": client_id(ask_id),
        "for": { "via": "chat", "login": login },
    });
    match board.op("idea_add", args).await {
        Ok(v) => Finished {
            idea_id: v.get("id").and_then(Value::as_str).map(str::to_string),
            ..Finished::done(format!("Filed as {}: {title}", if bug { "a bug" } else { "an idea" }))
        },
        Err(e) => Finished::failed(e),
    }
}

/// Posts a code question for a Coding Agent and waits for its row to finish: `wait` at most, looking every `poll`.
pub async fn ask_coding_agent<B: Board>(
    board: &B,
    ask_id: &str,
    question: &str,
    login: &str,
    wait: Duration,
    poll: Duration,
) -> Finished {
    let args = json!({
        "kind": "ask",
        "args": { "question": question, "codeQuestion": true, "askId": ask_id },
        "for": { "via": "chat", "login": login },
    });
    let id = match board.op("intent_request", args).await {
        Ok(v) => match v.get("id").and_then(Value::as_str) {
            Some(id) => id.to_string(),
            None => return Finished::failed("The backlog took the question but sent no row to wait for."),
        },
        Err(e) => return Finished::failed(e),
    };
    let until = tokio::time::Instant::now() + wait;
    while tokio::time::Instant::now() < until {
        // A read that fails is told, not retried quietly: the person is waiting on this.
        let rows = match board.op("intent_list", json!({ "kind": "ask" })).await {
            Ok(rows) => rows,
            Err(e) => return Finished::failed(e),
        };
        let row = rows.as_array().and_then(|rows| {
            rows.iter()
                .find(|r| r.get("id").and_then(Value::as_str) == Some(id.as_str()))
        });
        if let Some(row) = row {
            match row.get("status").and_then(Value::as_str) {
                Some("done") => {
                    let said = row.pointer("/result/answer").and_then(Value::as_str);
                    return Finished::done(said.unwrap_or("The Coding Agent had nothing to say."));
                }
                Some("refused" | "failed" | "timed_out" | "cancelled") => {
                    return Finished::failed(row.get("why").and_then(Value::as_str).unwrap_or(NONE_WAS_FREE));
                }
                _ => {}
            }
        }
        tokio::time::sleep(poll).await;
    }
    Finished::failed(NONE_WAS_FREE)
}

#[cfg(test)]
pub(crate) mod fake {
    use super::*;
    use std::sync::Mutex;

    /// A board that answers each op from a script and keeps what it was sent.
    type Reply = Box<dyn Fn(&str, &Value, usize) -> Result<Value, String> + Send + Sync>;

    pub struct Fake {
        pub sent: Mutex<Vec<(String, Value)>>,
        pub reply: Reply,
    }

    impl Fake {
        pub fn new(reply: impl Fn(&str, &Value, usize) -> Result<Value, String> + Send + Sync + 'static) -> Self {
            Fake {
                sent: Mutex::new(Vec::new()),
                reply: Box::new(reply),
            }
        }
        pub fn ops(&self) -> Vec<String> {
            self.sent.lock().unwrap().iter().map(|(o, _)| o.clone()).collect()
        }
    }

    impl Board for Fake {
        async fn op(&self, op: &'static str, args: Value) -> Result<Value, String> {
            // The fake keeps the real list too: a test that sends outside it fails like the store would.
            check_op(op, &args)?;
            let n = self.sent.lock().unwrap().len();
            let answer = (self.reply)(op, &args, n);
            self.sent.lock().unwrap().push((op.to_string(), args));
            answer
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::Fake;
    use super::*;

    fn chat_for() -> Value {
        json!({ "via": "chat", "login": "ada" })
    }

    #[test]
    fn reads_are_always_allowed_and_other_ops_are_not() {
        for op in READS {
            assert!(check_op(op, &json!({})).is_ok(), "{op}");
        }
        for op in [
            "idea_answer",
            "intent_claim",
            "pool_join",
            "jobs_due",
            "plan",
            "code_open",
            "draft_spec",
            "settings_set",
        ] {
            assert!(check_op(op, &json!({ "for": chat_for() })).is_err(), "{op}");
        }
    }

    #[test]
    fn idea_add_needs_for_chat_with_a_login() {
        assert!(check_op("idea_add", &json!({ "for": chat_for() })).is_ok());
        assert!(check_op("idea_add", &json!({})).is_err());
        assert!(check_op("idea_add", &json!({ "for": { "via": "chat" } })).is_err());
        assert!(check_op("idea_add", &json!({ "for": { "via": "chat", "login": " " } })).is_err());
        assert!(check_op("idea_add", &json!({ "for": { "via": "site", "login": "ada" } })).is_err());
    }

    #[test]
    fn intent_request_is_only_a_code_question() {
        let good = json!({ "kind": "ask", "args": { "question": "q", "codeQuestion": true, "askId": "a" }, "for": chat_for() });
        assert!(check_op("intent_request", &good).is_ok());
        for bad in [
            json!({ "kind": "play", "args": { "question": "q", "codeQuestion": true, "askId": "a" }, "for": chat_for() }),
            json!({ "kind": "ask", "args": { "question": "q", "askId": "a" }, "for": chat_for() }),
            json!({ "kind": "ask", "args": { "question": "q", "codeQuestion": true }, "for": chat_for() }),
            json!({ "kind": "ask", "args": { "codeQuestion": true, "askId": "a" }, "for": chat_for() }),
            json!({ "kind": "ask", "args": { "question": "q", "codeQuestion": true, "askId": "a" } }),
        ] {
            assert!(check_op("intent_request", &bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_client_id_is_lowercase_safe_and_at_most_sixty() {
        assert_eq!(client_id("Ask_1.X"), "chat-ask-1-x");
        assert_eq!(client_id(&"a".repeat(200)).len(), 60);
    }

    #[tokio::test]
    async fn a_bug_is_filed_for_the_asker_with_the_model_title() {
        let board = Fake::new(|_, _, _| Ok(json!({ "id": "idea-7" })));
        let r = file_idea(
            &board,
            "k1",
            "It crashes\nwhen I tap",
            Some("Crash on tap"),
            true,
            "ada",
        )
        .await;
        assert_eq!(r.answer, "Filed as a bug: Crash on tap");
        assert_eq!(r.idea_id.as_deref(), Some("idea-7"));
        let sent = board.sent.lock().unwrap();
        let (op, args) = &sent[0];
        assert_eq!(op, "idea_add");
        assert_eq!(args["experience"], "It crashes\nwhen I tap");
        assert_eq!(
            (
                args["freeText"].clone(),
                args["autoPlan"].clone(),
                args["askFirst"].clone(),
                args["bug"].clone()
            ),
            (json!(true), json!(true), json!(true), json!(true))
        );
        assert_eq!(args["clientId"], "chat-k1");
        assert_eq!(args["for"], chat_for());
    }

    #[tokio::test]
    async fn an_idea_without_a_title_takes_the_first_line_cut_to_eighty() {
        let board = Fake::new(|_, _, _| Ok(json!({})));
        let long = format!("{}\nmore", "w".repeat(100));
        let r = file_idea(&board, "k", &long, None, false, "ada").await;
        assert_eq!(r.answer, format!("Filed as an idea: {}", "w".repeat(80)));
        assert_eq!(r.idea_id, None);
    }

    #[tokio::test]
    async fn filing_for_nobody_is_refused_before_it_is_sent() {
        let board = Fake::new(|_, _, _| Ok(json!({})));
        let r = file_idea(&board, "k", "x", None, false, "").await;
        assert!(!r.done);
        assert!(board.ops().is_empty());
    }

    fn rows(status: &str, extra: Value) -> Value {
        let mut row = json!({ "id": "row-1", "status": status });
        row.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        json!([{ "id": "other", "status": "done" }, row])
    }

    #[tokio::test(start_paused = true)]
    async fn a_code_question_waits_for_its_own_row_and_returns_the_answer() {
        let board = Fake::new(|op, _, n| match op {
            "intent_request" => Ok(json!({ "id": "row-1" })),
            _ if n < 3 => Ok(rows("claimed", json!({}))),
            _ => Ok(rows("done", json!({ "result": { "answer": "In src/x.rs." } }))),
        });
        let r = ask_coding_agent(&board, "a1", "where is x?", "ada", CODE_QUESTION_WAIT, CODE_POLL).await;
        assert_eq!(r, Finished::done("In src/x.rs."));
        let sent = board.sent.lock().unwrap();
        assert_eq!(
            sent[0].1["args"],
            json!({ "question": "where is x?", "codeQuestion": true, "askId": "a1" })
        );
        assert_eq!(sent[1].1, json!({ "kind": "ask" }));
        assert_eq!(sent.len(), 4);
    }

    #[tokio::test(start_paused = true)]
    async fn a_done_row_with_no_answer_says_so() {
        let board = Fake::new(|op, _, _| match op {
            "intent_request" => Ok(json!({ "id": "row-1" })),
            _ => Ok(rows("done", json!({}))),
        });
        let r = ask_coding_agent(&board, "a", "q", "ada", CODE_QUESTION_WAIT, CODE_POLL).await;
        assert_eq!(r, Finished::done("The Coding Agent had nothing to say."));
    }

    #[tokio::test(start_paused = true)]
    async fn a_refused_row_gives_its_why_or_the_default() {
        for (extra, want) in [(json!({ "why": "No lane" }), "No lane"), (json!({}), NONE_WAS_FREE)] {
            let board = Fake::new(move |op, _, _| match op {
                "intent_request" => Ok(json!({ "id": "row-1" })),
                _ => Ok(rows("refused", extra.clone())),
            });
            let r = ask_coding_agent(&board, "a", "q", "ada", CODE_QUESTION_WAIT, CODE_POLL).await;
            assert_eq!(r, Finished::failed(want));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn five_minutes_with_no_answer_says_none_was_free() {
        let board = Fake::new(|op, _, _| match op {
            "intent_request" => Ok(json!({ "id": "row-1" })),
            _ => Ok(rows("open", json!({}))),
        });
        let start = tokio::time::Instant::now();
        let r = ask_coding_agent(&board, "a", "q", "ada", CODE_QUESTION_WAIT, CODE_POLL).await;
        assert_eq!(r, Finished::failed(NONE_WAS_FREE));
        assert!(start.elapsed() >= CODE_QUESTION_WAIT);
        assert!(start.elapsed() < CODE_QUESTION_WAIT + CODE_POLL * 2);
    }

    #[tokio::test]
    async fn a_failed_post_or_read_is_told() {
        let board = Fake::new(|_, _, _| Err("the store is down".into()));
        let r = ask_coding_agent(&board, "a", "q", "ada", CODE_QUESTION_WAIT, CODE_POLL).await;
        assert_eq!(r, Finished::failed("the store is down"));
        let board = Fake::new(|op, _, _| match op {
            "intent_request" => Ok(json!({ "id": "row-1" })),
            _ => Err("read failed".into()),
        });
        let r = ask_coding_agent(&board, "a", "q", "ada", CODE_QUESTION_WAIT, CODE_POLL).await;
        assert_eq!(r, Finished::failed("read failed"));
    }
}
