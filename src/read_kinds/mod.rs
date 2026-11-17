//! What a computer reads for the site (`read_file`, `issues_preview`; `agent/src/commands.ts` and the read kinds of
//! docs/specs/intent-rows.md section 5.2): a Markdown file from the base branch, GitHub's open issues for a label
//! or milestone with what importing each would do, and the Environments panel's view of staging and production. Each is
//! a read row of the store's `intents` fact (`intents::reads`); the hub no longer pushes them as commands. The one read
//! it still pushes is the Releases tab's `release_status` ([`release`], not a row).
//!
//! A read that can't be answered is a [`ReadError`]: `Refused` is the computer's own no in plain words (the TS
//! agent's `HttpError`), `Failed` is something that broke.
use std::sync::Arc;

use serde_json::Value;

use crate::backlog::Client;
use crate::config::ProjectConfig;

pub mod environments;
pub mod file;
pub mod issues;
pub mod release;

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
    /// The project's deploy log, for the Environments panel's last deploys (the pushed command only).
    pub deploy_log: Option<Arc<crate::promote::DeployLog>>,
}

/// The kinds of read this computer answers as intent rows (a read row's result may hold 200 KB).
pub const KINDS: &[&str] = &["read_file", "issues_preview", "environments"];

/// Runs the read `kind` with the command's own `args`.
pub async fn read(kind: &str, args: &Value, cx: &Reading) -> Result<Value, ReadError> {
    match kind {
        "read_file" => file::read_file(&cx.project, args),
        "issues_preview" => issues::issues_preview(cx, args).await,
        "environments" => environments::environments(cx).await,
        // Not a row: the Releases tab's reads of the saved notes, which the hub relays as a command.
        "release_status" => release::release_status(cx, args).await,
        other => Err(ReadError::Refused(format!(
            "Not a command this agent takes yet: {other}"
        ))),
    }
}

#[cfg(test)]
pub(crate) mod tests;
