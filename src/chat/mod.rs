//! The Chat Agent (Retaskable.com docs/specs/chat-agent-process.md, chat-classifier.md): a separate agent, the
//! `retaskable-chat-agent` binary, that serves one person's chat. It links as `role: "chat"`, takes only `ask`
//! commands, sorts each message with one Haiku call, files a bug or idea on the shared backlog, hands a code question
//! to a Coding Agent as an `ask` row, and answers anything else from the board. It has no lanes, worktrees or code
//! sessions, and it holds no secret and no server logic: the server enforces what it may write ([`board::check_op`]
//! is only the same list kept here so a bug cannot try more).
//!
//! No fallback anywhere: when the classifier cannot run, the person is told why and nothing is filed or guessed.
use serde_json::{Value, json};

pub mod answer;
pub mod ask;
pub mod board;
pub mod classify;
pub mod claude;
pub mod hub;
pub mod link;
pub mod project;
pub mod report;

/// The role this agent links and says hello with.
pub const ROLE: &str = "chat";
/// What a hello's `capabilities` lists once `ask` takes `context` (shared/storeAsks.ts `CHAT_CONTEXT_CAPABILITY`).
pub const CHAT_CONTEXT_CAPABILITY: &str = "chat_context";
/// The OS credential service the Chat Agent's links are kept under: its own identity, not the Coding Agent's.
pub const KEYCHAIN_SERVICE: &str = "retaskable-chat";
/// The file that overrides where the Chat Agent keeps its link tokens.
pub const CREDENTIALS_FILE_VAR: &str = "RETASKABLE_CHAT_CREDENTIALS_FILE";
/// The Chat Agent's own folder inside the data folder, so its credentials file and config never meet the Coding Agent's.
pub const DATA_SUBDIR: &str = "chat";

/// The reply to a command the hub sent: `{type:"ack", id}`.
pub fn ack(id: &str) -> Value {
    json!({ "type": "ack", "id": id })
}

/// `{type:"nack", id, reason}`.
pub fn nack(id: &str, reason: &str) -> Value {
    json!({ "type": "nack", "id": id, "reason": reason })
}

/// The one command the Chat Agent takes (chat-agent-process.md section 4).
pub const ONLY_ASKS: &str = "The Chat Agent takes only chat messages.";
