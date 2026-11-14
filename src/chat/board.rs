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

/// A store picture id (`PICTURE_ID`, shared/sentPictures.ts): a uuid and an image extension.
pub fn picture_id(id: &str) -> bool {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"^[0-9a-f-]{36}\.(png|jpg|gif|webp)$").expect("a valid pattern"))
        .is_match(id)
}

/// At most this many pictures go with a message (`MAX_SENT_PICTURES`).
pub const MAX_PICTURES: usize = 4;

/// `pictureIds` absent, or an array of at most [`MAX_PICTURES`] valid store ids.
fn pictures_ok(v: Option<&Value>) -> bool {
    match v {
        None => true,
        Some(v) => v
            .as_array()
            .is_some_and(|a| a.len() <= MAX_PICTURES && a.iter().all(|x| x.as_str().is_some_and(picture_id))),
    }
}

/// The allowlist (`chatOpRefusal`): `Err` says why the op is not the Chat Agent's to send.
pub fn check_op(op: &str, args: &Value) -> Result<(), String> {
    if READS.contains(&op) {
        return Ok(());
    }
    match op {
        "idea_add" if for_chat(args) && !pictures_ok(args.get("pictureIds")) => {
            Err("pictureIds must be up to 4 pictures' ids in the store".into())
        }
        "idea_add" if for_chat(args) => Ok(()),
        "idea_add" => Err("idea_add is only for the person who typed the message (for: via chat, with a login)".into()),
        "intent_request" => {
            let nested = args.get("args");
            let ok = for_chat(args)
                && args.get("kind").and_then(Value::as_str) == Some("ask")
                && nested.and_then(|a| a.get("codeQuestion")) == Some(&Value::Bool(true))
                && nested.and_then(|a| a.get("question")).is_some_and(Value::is_string)
                && nested.and_then(|a| a.get("askId")).is_some_and(Value::is_string)
                && pictures_ok(nested.and_then(|a| a.get("pictureIds")));
            if ok {
                Ok(())
            } else {
                Err("intent_request is only a code question: kind ask, codeQuestion true, a question and an askId (pictureIds, if any, valid store ids), for the asker".into())
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

/// What a filed idea carries from the ask beside its words: the message's pictures (store ids), the ask's audience
/// (`Some(None)` is "nobody", absent is the project's guess) and the card it was typed on (chat-classifier.md section 4).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Carry {
    pub picture_ids: Vec<String>,
    pub audience: Option<Option<String>>,
    pub about: Option<Value>,
}

impl Carry {
    /// `about` as `{ kind, id }`, only when it is a card or plan as the store names them.
    fn about(&self) -> Option<Value> {
        let a = self.about.as_ref()?;
        let kind = a
            .get("kind")
            .and_then(Value::as_str)
            .filter(|k| matches!(*k, "item" | "draft"))?;
        let id = a
            .get("id")
            .and_then(Value::as_str)
            .filter(|i| !i.is_empty() && i.len() <= 80)?;
        Some(json!({ "kind": kind, "id": id }))
    }
}

/// Files the message as the asker's own bug or idea, planned automatically and asked about first.
pub async fn file_idea<B: Board>(
    board: &B,
    ask_id: &str,
    question: &str,
    title: Option<&str>,
    bug: bool,
    login: &str,
    carry: &Carry,
) -> Finished {
    let first_line = question.lines().next().unwrap_or_default().trim();
    let title: String = title
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or(first_line)
        .chars()
        .take(80)
        .collect();
    let mut args = json!({
        "title": title,
        "experience": question,
        "freeText": true,
        "autoPlan": true,
        "askFirst": true,
        "bug": bug,
        "clientId": client_id(ask_id),
        "for": { "via": "chat", "login": login },
    });
    let o = args.as_object_mut().expect("an object");
    if !carry.picture_ids.is_empty() {
        o.insert("pictureIds".into(), json!(carry.picture_ids));
    }
    if let Some(audience) = &carry.audience {
        o.insert("audience".into(), json!(audience));
    }
    if let Some(about) = carry.about() {
        // A card's own item is what the request revises (card-revisions.md section 3.2); a plan's page is new work.
        if about["kind"] == "item" {
            o.insert("revises".into(), about["id"].clone());
        }
        o.insert("about".into(), about);
    }
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
    picture_ids: &[String],
    wait: Duration,
    poll: Duration,
) -> Finished {
    let mut args = json!({
        "kind": "ask",
        "args": { "question": question, "codeQuestion": true, "askId": ask_id },
        "for": { "via": "chat", "login": login },
    });
    if !picture_ids.is_empty() {
        args["args"]["pictureIds"] = json!(picture_ids);
    }
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

    const PIC: &str = "01234567-89ab-cdef-0123-456789abcdef.png";

    #[test]
    fn pictures_are_allowed_only_as_valid_store_ids() {
        assert!(picture_id("01234567-89ab-cdef-0123-456789abcdef.webp"));
        for bad in ["x.png", "../etc/passwd", "01234567-89ab-cdef-0123-456789abcdef.svg"] {
            assert!(!picture_id(bad), "{bad}");
        }
        let idea = |p: Value| json!({ "pictureIds": p, "for": chat_for() });
        assert!(check_op("idea_add", &idea(json!([PIC]))).is_ok());
        for bad in [
            json!(["nope"]),
            json!("x"),
            json!([1]),
            json!([PIC, PIC, PIC, PIC, PIC]),
        ] {
            assert!(check_op("idea_add", &idea(bad.clone())).is_err(), "{bad}");
        }
        let ask = |p: Value| json!({ "kind": "ask", "args": { "question": "q", "codeQuestion": true, "askId": "a", "pictureIds": p }, "for": chat_for() });
        assert!(check_op("intent_request", &ask(json!([PIC]))).is_ok());
        assert!(check_op("intent_request", &ask(json!(["../x"]))).is_err());
    }

    #[tokio::test]
    async fn a_filed_idea_carries_pictures_audience_and_its_card() {
        let board = Fake::new(|_, _, _| Ok(json!({ "id": "idea-1" })));
        let carry = Carry {
            picture_ids: vec![PIC.to_string()],
            audience: Some(Some("owners".into())),
            about: Some(json!({ "kind": "item", "id": "i1", "extra": 1 })),
        };
        assert!(file_idea(&board, "k", "x", None, false, "ada", &carry).await.done);
        // Audience "nobody" is sent as null; a plan's page is about, not a revision; none of it is invented.
        let carry2 = Carry {
            audience: Some(None),
            about: Some(json!({ "kind": "draft", "id": "d1" })),
            ..Carry::default()
        };
        file_idea(&board, "k", "x", None, false, "ada", &carry2).await;
        file_idea(&board, "k", "x", None, false, "ada", &Carry::default()).await;
        let sent = board.sent.lock().unwrap();
        let a = &sent[0].1;
        assert_eq!(
            (a["pictureIds"].clone(), a["audience"].clone(), a["revises"].clone()),
            (json!([PIC]), json!("owners"), json!("i1"))
        );
        assert_eq!(a["about"], json!({ "kind": "item", "id": "i1" }));
        assert_eq!(
            (sent[1].1["audience"].clone(), sent[1].1.get("revises")),
            (Value::Null, None)
        );
        assert_eq!(sent[1].1["about"]["kind"], "draft");
        for k in ["pictureIds", "audience", "about", "revises"] {
            assert!(sent[2].1.get(k).is_none(), "{k}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_code_question_names_the_pictures_by_id() {
        let board = Fake::new(|op, _, _| match op {
            "intent_request" => Ok(json!({ "id": "row-1" })),
            _ => Ok(rows("done", json!({}))),
        });
        let pics = [PIC.to_string()];
        ask_coding_agent(&board, "a1", "see?", "ada", &pics, CODE_QUESTION_WAIT, CODE_POLL).await;
        assert_eq!(board.sent.lock().unwrap()[0].1["args"]["pictureIds"], json!([PIC]));
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
            &Carry::default(),
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
        let r = file_idea(&board, "k", &long, None, false, "ada", &Carry::default()).await;
        assert_eq!(r.answer, format!("Filed as an idea: {}", "w".repeat(80)));
        assert_eq!(r.idea_id, None);
    }

    #[tokio::test]
    async fn filing_for_nobody_is_refused_before_it_is_sent() {
        let board = Fake::new(|_, _, _| Ok(json!({})));
        let r = file_idea(&board, "k", "x", None, false, "", &Carry::default()).await;
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
        let r = ask_coding_agent(&board, "a1", "where is x?", "ada", &[], CODE_QUESTION_WAIT, CODE_POLL).await;
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
        let r = ask_coding_agent(&board, "a", "q", "ada", &[], CODE_QUESTION_WAIT, CODE_POLL).await;
        assert_eq!(r, Finished::done("The Coding Agent had nothing to say."));
    }

    #[tokio::test(start_paused = true)]
    async fn a_refused_row_gives_its_why_or_the_default() {
        for (extra, want) in [(json!({ "why": "No lane" }), "No lane"), (json!({}), NONE_WAS_FREE)] {
            let board = Fake::new(move |op, _, _| match op {
                "intent_request" => Ok(json!({ "id": "row-1" })),
                _ => Ok(rows("refused", extra.clone())),
            });
            let r = ask_coding_agent(&board, "a", "q", "ada", &[], CODE_QUESTION_WAIT, CODE_POLL).await;
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
        let r = ask_coding_agent(&board, "a", "q", "ada", &[], CODE_QUESTION_WAIT, CODE_POLL).await;
        assert_eq!(r, Finished::failed(NONE_WAS_FREE));
        assert!(start.elapsed() >= CODE_QUESTION_WAIT);
        assert!(start.elapsed() < CODE_QUESTION_WAIT + CODE_POLL * 2);
    }

    #[tokio::test]
    async fn a_failed_post_or_read_is_told() {
        let board = Fake::new(|_, _, _| Err("the store is down".into()));
        let r = ask_coding_agent(&board, "a", "q", "ada", &[], CODE_QUESTION_WAIT, CODE_POLL).await;
        assert_eq!(r, Finished::failed("the store is down"));
        let board = Fake::new(|op, _, _| match op {
            "intent_request" => Ok(json!({ "id": "row-1" })),
            _ => Err("read failed".into()),
        });
        let r = ask_coding_agent(&board, "a", "q", "ada", &[], CODE_QUESTION_WAIT, CODE_POLL).await;
        assert_eq!(r, Finished::failed("read failed"));
    }
}
