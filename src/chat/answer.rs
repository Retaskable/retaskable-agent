//! Answering a question from the board (assistant.ts `answer`, as the Chat Agent has it): the prompt is the assistant's,
//! the state in it is what the store's reads give, and the earlier exchanges are the `context` the hub put on the ask.
//! The Chat Agent has no checkout and no tools, so it says only what that state says, and it carries out no action: any
//! the model returns are dropped.
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

use super::board::{Board, Finished};
use super::classify::{Reason, failure_reason, unavailable_text};
use super::claude::Runner;
use crate::engines::{CouldNotStart, EngineId, ephemeral_readonly_spec};
use crate::prompt::render_prompt;

pub const TEMPLATE: &str = include_str!("../../prompts/chat/assistant.md");
pub const TIMEOUT: Duration = Duration::from_secs(120);
/// Rows of each list the state carries, newest as the store gave them.
const ROWS: usize = 40;

const ROLE_NOTE: &str = "You are the Chat Agent, which only answers. You have no checkout, no files and no tools, \
and you cannot carry out any action above: `actions` must always be an empty list. If the person wants something done, \
say in one sentence that the Chat Agent can't do it and that the buttons on the page can. Answer only from the state \
below and the conversation; if they don't say, say you don't know.";

/// The project this chat is on, as the prompt names it.
#[derive(Debug, Clone, Default)]
pub struct Project {
    pub github_repo: String,
    pub base_branch: String,
}

/// An earlier exchange from the ask's `context` (shared/cloudCommands.ts `chatContext`).
#[derive(Debug, Clone, PartialEq)]
pub struct Exchange {
    pub question: String,
    pub answer: String,
    pub from_retaskable: bool,
    /// `<type> ok|failed: <message>` for each action that came of it.
    pub outcomes: Vec<String>,
}

/// The `context` field read tolerantly: whatever does not fit is left out.
pub fn exchanges(context: &Value) -> Vec<Exchange> {
    let text = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    context
        .as_array()
        .map(|list| {
            list.iter()
                .map(|e| Exchange {
                    question: text(e, "question"),
                    answer: text(e, "answer"),
                    from_retaskable: e.get("from").and_then(Value::as_str) == Some("retaskable"),
                    outcomes: e
                        .get("outcomes")
                        .and_then(Value::as_array)
                        .map(|o| {
                            o.iter()
                                .map(|x| {
                                    format!(
                                        "{} {}: {}",
                                        x.pointer("/action/type").and_then(Value::as_str).unwrap_or("?"),
                                        if x.get("ok").and_then(Value::as_bool) == Some(true) {
                                            "ok"
                                        } else {
                                            "failed"
                                        },
                                        text(x, "message")
                                    )
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `exchanges()` in assistant.ts: the history the prompt shows.
pub fn history(list: &[Exchange]) -> String {
    if list.is_empty() {
        return "(none)".to_string();
    }
    list.iter()
        .map(|e| {
            if e.from_retaskable {
                format!("Retaskable: {}", e.answer)
            } else {
                let actions = if e.outcomes.is_empty() {
                    String::new()
                } else {
                    format!("\n(Actions: {})", e.outcomes.join("; "))
                };
                format!("Human: {}\nYou: {}{actions}", e.question, e.answer)
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn lines(rows: &Value, title: &str) -> String {
    let Some(list) = rows.as_array() else {
        return "(none)".to_string();
    };
    let mut out: Vec<String> = list
        .iter()
        .take(ROWS)
        .map(|r| {
            let field = |k: &str| r.get(k).and_then(Value::as_str).filter(|s| !s.is_empty());
            let name = field("title").or_else(|| field("name")).unwrap_or(title);
            match (field("status").or_else(|| field("state")), field("id")) {
                (Some(s), Some(id)) => format!("- {name} ({s}) [{id}]"),
                (Some(s), None) => format!("- {name} ({s})"),
                (None, Some(id)) => format!("- {name} [{id}]"),
                (None, None) => format!("- {name}"),
            }
        })
        .collect();
    if list.len() > ROWS {
        out.push(format!("- and {} more", list.len() - ROWS));
    }
    if out.is_empty() {
        "(none)".to_string()
    } else {
        out.join("\n")
    }
}

/// The state the prompt carries, read from the store. The backlog's own list must read: with none, nothing is
/// answered from a guess. The others are told as unreadable when they fail.
pub async fn board_state<B: Board>(board: &B) -> Result<String, String> {
    let items = board.op("list", json!({})).await?;
    let mut state = format!("{ROLE_NOTE}\n\nBacklog items:\n{}", lines(&items, "(untitled)"));
    for (op, heading) in [("drafts", "Spec drafts"), ("ideas_list", "Ideas")] {
        match board.op(op, json!({})).await {
            Ok(rows) => state.push_str(&format!("\n\n{heading}:\n{}", lines(&rows, "(untitled)"))),
            Err(e) => state.push_str(&format!("\n\n{heading}: couldn't be read ({e})")),
        }
    }
    Ok(state)
}

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": { "answer": { "type": "string" }, "actions": { "type": "array" } },
        "required": ["answer", "actions"]
    })
}

/// Answers `question` from the board and `context`. A session that cannot run says why; nothing is guessed.
pub async fn answer<B: Board, R: Runner>(
    board: &B,
    runner: &R,
    project: &Project,
    question: &str,
    context: &[Exchange],
    cwd: &Path,
    host: &str,
) -> Finished {
    if !runner.available() {
        return Finished::failed(unavailable_text(Reason::NoClaude, host));
    }
    let state = match board_state(board).await {
        Ok(s) => s,
        Err(e) => return Finished::failed(format!("Chat Agent couldn't read the board, so it didn't answer: {e}")),
    };
    let branch = if project.base_branch.is_empty() {
        "main"
    } else {
        &project.base_branch
    };
    let prompt = render_prompt(
        TEMPLATE,
        &[
            ("pr", "pull request".to_string()),
            ("projectKind", "a software project".to_string()),
            ("projectNoun", "the app".to_string()),
            ("baseBranch", branch.to_string()),
            ("githubRepo", project.github_repo.clone()),
            ("state", state),
            ("history", history(context)),
            ("question", question.to_string()),
            ("change", "pull request".to_string()),
        ],
    );
    let Ok(prompt) = prompt else {
        return Finished::failed(unavailable_text(Reason::Unreadable, host));
    };
    let mut spec = ephemeral_readonly_spec(
        Some(EngineId::Claude),
        "sonnet".to_string(),
        prompt,
        schema(),
        cwd.to_path_buf(),
        0.0,
        None,
        None,
        false,
        false,
    );
    spec.max_turns = Some(3);
    let result = match tokio::time::timeout(TIMEOUT, runner.run(spec)).await {
        Err(_) => return Finished::failed(unavailable_text(Reason::Timeout, host)),
        Ok(Err(e)) if e.downcast_ref::<CouldNotStart>().is_some() => {
            return Finished::failed(unavailable_text(Reason::NoClaude, host));
        }
        Ok(Err(_)) => return Finished::failed(unavailable_text(Reason::Unreadable, host)),
        Ok(Ok(r)) => r,
    };
    if !result.ok {
        return Finished::failed(unavailable_text(failure_reason(&result), host));
    }
    match result
        .structured
        .as_ref()
        .and_then(|v| v.get("answer"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|a| !a.is_empty())
    {
        Some(text) => Finished::done(text),
        None => Finished::failed(unavailable_text(Reason::Unreadable, host)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::board::fake::Fake as FakeBoard;
    use crate::chat::claude::fake::Fake;
    use crate::engines::engine::test_result;

    fn board() -> FakeBoard {
        FakeBoard::new(|op, _, _| match op {
            "list" => Ok(json!([{ "id": "i1", "title": "Dark mode", "status": "building" }])),
            "drafts" => Ok(json!([])),
            _ => Ok(json!([{ "title": "Faster start" }])),
        })
    }

    fn said(v: Value) -> Fake {
        Fake::new(vec![Ok(test_result(true, "success", Some(v), None))])
    }

    #[test]
    fn the_context_reads_into_the_assistants_history_lines() {
        let ctx = json!([
            { "question": "why?", "answer": "Because.", "outcomes": [{ "action": { "type": "pause" }, "ok": true, "message": "Paused" }] },
            { "question": "", "answer": "Filed it", "from": "retaskable", "outcomes": [] },
            "junk"
        ]);
        let list = exchanges(&ctx);
        assert_eq!(
            history(&list[..2]),
            "Human: why?\nYou: Because.\n(Actions: pause ok: Paused)\n\nRetaskable: Filed it"
        );
        assert_eq!(history(&[]), "(none)");
        assert!(exchanges(&json!("x")).is_empty());
    }

    #[tokio::test]
    async fn a_question_is_answered_from_the_boards_state_with_no_actions() {
        let runner = said(json!({ "answer": "Dark mode is being built.", "actions": [{ "type": "pause" }] }));
        let ctx = [Exchange {
            question: "hi".into(),
            answer: "hello".into(),
            from_retaskable: false,
            outcomes: vec![],
        }];
        let project = Project {
            github_repo: "o/r".into(),
            base_branch: "main".into(),
        };
        let r = answer(
            &board(),
            &runner,
            &project,
            "what's building?",
            &ctx,
            Path::new("."),
            "box",
        )
        .await;
        assert_eq!(r, Finished::done("Dark mode is being built."));
        let specs = runner.specs.lock().unwrap();
        let p = &specs[0].prompt;
        assert!(p.contains("- Dark mode (building) [i1]") && p.contains("- Faster start"));
        assert!(p.contains("Human: hi\nYou: hello") && p.contains("what's building?") && p.contains("o/r"));
        assert!(p.contains("Chat Agent, which only answers"));
        assert!(!p.contains("{{"));
        assert_eq!(specs[0].model, "sonnet");
    }

    #[tokio::test]
    async fn a_board_that_cannot_be_read_answers_nothing() {
        let runner = Fake::new(vec![]);
        let down = FakeBoard::new(|_, _, _| Err("store down".into()));
        let r = answer(&down, &runner, &Project::default(), "q", &[], Path::new("."), "box").await;
        assert!(!r.done && r.answer.contains("store down"));
        assert!(runner.specs.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_unreadable_side_list_is_said_in_the_state_not_hidden() {
        let b = FakeBoard::new(|op, _, _| {
            if op == "list" {
                Ok(json!([]))
            } else {
                Err("nope".into())
            }
        });
        let state = board_state(&b).await.unwrap();
        assert!(state.contains("Spec drafts: couldn't be read (nope)"));
    }

    #[tokio::test]
    async fn a_session_that_cannot_run_says_why_and_never_answers() {
        let signed_out = test_result(false, "error", None, Some("x"));
        let runner = Fake::new(vec![Ok(crate::engines::SessionResult {
            signed_out: true,
            ..signed_out
        })]);
        let r = answer(&board(), &runner, &Project::default(), "q", &[], Path::new("."), "box").await;
        assert_eq!(r, Finished::failed(unavailable_text(Reason::SignedOut, "box")));
        let mut none = Fake::new(vec![]);
        none.available = false;
        let r = answer(&board(), &none, &Project::default(), "q", &[], Path::new("."), "box").await;
        assert_eq!(r, Finished::failed(unavailable_text(Reason::NoClaude, "box")));
        let r = answer(
            &board(),
            &said(json!({ "answer": "  ", "actions": [] })),
            &Project::default(),
            "q",
            &[],
            Path::new("."),
            "box",
        )
        .await;
        assert!(!r.done);
    }
}
