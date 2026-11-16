//! What a computer reads for the site (`read_file`, `issues_preview`; `agent/src/commands.ts` and the read kinds of
//! docs/specs/intent-rows.md section 5.2): a Markdown file from the base branch, and GitHub's open issues for a label
//! or milestone with what importing each would do. Each is asked two ways, and both run the same read: as the pushed
//! command the hub sends today (answered in the `ack`'s `result`), and as a read row of the store's `intents` fact
//! (`intents::reads`), which the site will write once the hub does.
//!
//! A read that can't be answered is a [`ReadError`]: `Refused` is the computer's own no in plain words (the TS
//! agent's `HttpError`), `Failed` is something that broke.
use std::sync::Arc;

use serde_json::Value;

use crate::backlog::Client;
use crate::config::ProjectConfig;

pub mod file;
pub mod issues;

/// Why a read wasn't answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadError {
    Refused(String),
    Failed(String),
}

impl ReadError {
    /// The words, whichever it is.
    pub fn why(&self) -> &str {
        match self {
            Self::Refused(w) | Self::Failed(w) => w,
        }
    }
}

/// What a read needs: the project's checkout and settings, and the store (its drafts, for Show matches).
#[derive(Clone)]
pub struct Reading {
    pub client: Arc<Client>,
    pub repo: u64,
    pub token: String,
    pub project: ProjectConfig,
    /// The `gh` program (a test passes a fake).
    pub gh: std::path::PathBuf,
}

/// The kinds of read this computer answers.
pub const KINDS: &[&str] = &["read_file", "issues_preview"];

/// Runs the read `kind` with the command's own `args`.
pub async fn read(kind: &str, args: &Value, cx: &Reading) -> Result<Value, ReadError> {
    match kind {
        "read_file" => file::read_file(&cx.project, args),
        "issues_preview" => issues::issues_preview(cx, args).await,
        other => Err(ReadError::Refused(format!(
            "Not a command this agent takes yet: {other}"
        ))),
    }
}

#[cfg(test)]
mod tests;
