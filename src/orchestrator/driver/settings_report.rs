//! `settings_report`: tells the store what this computer runs of the project's shared settings (the TS agent's
//! `settingsReport` and `reportSettings`, `agent/src/storeLink.ts`; the store's side is `settings_report` in
//! `shared/backlogTeamOps.ts` and `applyReport` in `shared/projectSettings.ts`). The site shows it per computer
//! (Settings, Computers): the commands version this computer allows and the settings version it has applied.
//!
//! Sent when the report changed and at least once a day, so the computer stays counted; one the store didn't take is
//! sent again on the next pass. Nothing is sent before the store has settings for the project.
//!
//! Differs from the TS agent: there is no `denied` (nothing here denies a version: it waits until approved, and the
//! `denied` field is from before 2026-10-22) and no `baseFailed` (the base branch is config.json's, never the store's, so
//! there is none to fail to check out). `migrated` is always true: this agent has no old copy of the project's
//! settings to move, so it never holds back another computer's `everyComputerMoved` (`shared/projectSettings.ts`).
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::ProjectLoop;
use crate::backlog::access::{self, AccessTokens};

/// How often a computer tells the store it is still there when nothing changed (`REPORT_EVERY_MS`).
const REPORT_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// What this project's loop knows of its settings report.
#[derive(Debug, Default)]
pub(super) struct SettingsReport {
    /// The `version` of the settings record the last read applied; `None` while the store has none.
    pub(super) applied: Option<u64>,
    /// The report the store last took, and when.
    sent: Option<(String, Instant)>,
}

/// The op's args: `{ machine, allowed, appliedVersion, migrated }`. `allowed` is the commands version this
/// computer runs (0 before it has allowed any).
pub(crate) fn report_args(machine: &str, allowed: u64, applied: u64) -> Value {
    json!({
        "machine": crate::text::truncate(machine, 200),
        "allowed": allowed,
        "appliedVersion": applied,
        "migrated": true,
    })
}

impl SettingsReport {
    /// The args to send now, or `None`: nothing to say yet, or the store already has this one and it is not a day old.
    fn due(&mut self, machine: &str, allowed: u64, now: Instant) -> Option<(Value, String)> {
        let args = report_args(machine, allowed, self.applied?);
        let key = args.to_string();
        match &self.sent {
            Some((k, at)) if *k == key && now.duration_since(*at) < REPORT_EVERY => None,
            _ => Some((args, key)),
        }
    }
}

impl ProjectLoop {
    /// Sends the settings report when it is due. A failure is logged and the report sent again next pass.
    pub(super) async fn report_settings(
        &mut self,
        commands: &crate::view::ItemCommands,
        access: &dyn AccessTokens,
        machine: &str,
    ) {
        let allowed = commands.gates().allowed(&self.id).map_or(0, |a| a.commands_version);
        let Some((args, key)) = self.settings_report.due(machine, allowed, Instant::now()) else {
            return;
        };
        let Some(repo) = self.repo else { return };
        let Ok(Some(token)) = access::current(access).await else {
            return;
        };
        let send = self.client.op_done(repo, Some(&token), "settings_report", args);
        match tokio::time::timeout(Duration::from_secs(15), send).await {
            Ok(Ok(())) => self.settings_report.sent = Some((key, Instant::now())),
            Ok(Err(e)) => tracing::warn!(
                "{}: couldn't tell the store what this computer runs of the settings: {e}",
                self.id
            ),
            Err(_) => tracing::warn!("{}: settings_report timed out", self.id),
        }
    }
}

#[cfg(test)]
mod tests;
