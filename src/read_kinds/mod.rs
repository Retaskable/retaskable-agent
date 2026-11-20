//! What a computer reads for the site (`read_file`, `issues_preview`; `agent/src/commands.ts` and the read kinds of
//! docs/specs/intent-rows.md section 5.2): a Markdown file from the base branch, GitHub's open issues for a label
//! or milestone with what importing each would do, and the Environments panel's view of staging and production. Each is
//! a read row of the store's `intents` fact (`intents::reads`); the hub no longer pushes them as commands. The kept-fact
//! reads `draft_files`, `release_status` and `main_pr_draft` are rows too, when the hub holds no fact, and it still pushes
//! each of them as well; the walkthrough's `go_live` and Process's `confirm` ([`live`]) are only pushed.
//!
//! A read that can't be answered is a [`ReadError`]: `Refused` is the computer's own no in plain words (the TS
//! agent's `HttpError`), `Failed` is something that broke.
use std::sync::Arc;

use serde_json::Value;

use crate::backlog::Client;
use crate::config::ProjectConfig;

pub mod draft_files;
pub mod environments;
pub mod file;
pub mod issues;
pub mod live;
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
    /// The facts this project keeps for the site: a draft's files not yet delivered (`draft_files`, the pushed command).
    pub facts: Option<crate::kept_facts::KeptFacts>,
}

/// The kinds of read this computer answers as intent rows (a read row's result may hold 200 KB), here and in any
/// project's puller: `main_pr_draft` is the project's loop's (it knows the dev branch), so it isn't in this list.
pub const KINDS: &[&str] = &[
    "read_file",
    "issues_preview",
    "environments",
    "draft_files",
    "release_status",
];

/// Runs the read `kind` with the command's own `args`.
pub async fn read(kind: &str, args: &Value, cx: &Reading) -> Result<Value, ReadError> {
    match kind {
        "read_file" => file::read_file(&cx.project, args),
        "issues_preview" => issues::issues_preview(cx, args).await,
        "environments" => environments::environments(cx).await,
        // A row, and still the Releases tab's pushed command.
        "release_status" => release::release_status(cx, args).await,
        // Only pushed: the commands the hub still sends that ask the store, or the web, one thing.
        "go_live" => live::go_live(cx, args).await,
        "confirm" => live::confirm(cx, args).await,
        // A row (for a plan this computer holds), and still pushed when the hub has no kept fact.
        "draft_files" => draft_files::draft_files(cx, args).await,
        other => Err(ReadError::Refused(format!(
            "Not a command this agent takes yet: {other}"
        ))),
    }
}

#[cfg(test)]
pub(crate) mod tests;
