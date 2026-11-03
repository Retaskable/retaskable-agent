//! Reporting the Retaskable bug a look named (docs/specs/pieces.md section 3.5, "reported as the triager's is"):
//! scrubbed of the project's, the piece's and the computer's words, decided against the hub's consent and known
//! bugs (a `sameAs` naming a known one is already on the board, so nothing goes), left out when an earlier look in
//! the brief already gave the same signature, and sent from its own task so the run's end isn't held. The hub's
//! answer, or why nothing was sent, is logged.
use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;
use tokio::task::JoinHandle;

use crate::config::ProjectConfig;
use crate::connector::bug_link::{BugLink, BugOutcome};
use crate::env::Env;
use crate::looks::LookResult;
use crate::orchestrator::driver::{RunRequest, piece_id};
use crate::triage::RetaskableBug;
use crate::triage::bug_report::{decide_bug_report, prepare_bug_report, scrub_words};

/// Where a look's bug goes: each project's hub link, and the version a report says the build is.
#[derive(Clone)]
pub struct BugReports {
    pub links: HashMap<String, Arc<BugLink>>,
    pub version: String,
    pub env: Env,
}

impl BugReports {
    /// Decides and starts sending the bug `result` names for `request`'s piece, if any; the handle ends with the send.
    pub(super) fn report(
        &self,
        cfg: &ProjectConfig,
        request: &RunRequest,
        result: &LookResult,
    ) -> Option<JoinHandle<()>> {
        let bug = result.retaskable_bug.as_ref()?;
        let (project, piece_id) = (request.project.as_str(), piece_id(&request.piece));
        let Some(link) = self.links.get(project).cloned() else {
            tracing::info!("{project}: no bug report for {piece_id}: the project has no hub link");
            return None;
        };
        let words = scrub_words(project, cfg, piece_id, request.piece.branch.as_deref(), &self.env);
        let given: Vec<String> = request
            .piece
            .other
            .get("looks")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|l| {
                let b = l.get("retaskableBug")?;
                let text = |k: &str| b.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
                let earlier = RetaskableBug {
                    title: text("title"),
                    details: text("details"),
                    same_as: None,
                };
                let why = l.get("reason").and_then(Value::as_str);
                prepare_bug_report(&earlier, why, &words).map(|p| p.signature)
            })
            .collect();
        let message = decide_bug_report(
            Some(bug),
            Some(&result.reason),
            &words,
            link.consent(),
            None,
            &link.known(),
        )
        .filter(|m| m.same_as.is_none() && !given.contains(&m.signature));
        let Some(message) = message else {
            tracing::info!("{project}: no bug report sent for {piece_id}: not agreed, empty or already reported");
            return None;
        };
        let version = self.version.clone();
        let project = project.to_string();
        Some(tokio::spawn(async move {
            match link.send(&message, &version).await {
                BugOutcome::Answered(a) if a.ok => tracing::info!(
                    "{project}: bug report sent{}",
                    if a.duplicate {
                        " (already on Retaskable's board)"
                    } else {
                        ""
                    }
                ),
                BugOutcome::Answered(a) => tracing::info!("{project}: bug report refused: {:?}", a.reason),
                BugOutcome::NotSent(why) => tracing::info!("{project}: bug report not sent: {why}"),
            }
        }))
    }
}
