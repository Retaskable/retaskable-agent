//! `backlog_link {}` and `settings_request { ... }` (`agent/src/commands.ts`): what retaskable.com may ask of this
//! computer's own links and settings.
//!
//! **backlog_link** (Link again) starts this computer's device flow to the shared backlog, as the tray's button does: the
//! approval page opens here, and a flow already waiting is left to wait. It `ack`s once started; who approves is the person
//! at the computer.
//!
//! **settings_request** takes only `keepBranches` from the site since 2026-10-22: every other field is the project's, saved
//! in the store (`projectFieldsRefusal`, `shared/projectSettings.ts`), and the lanes are the computer's. `keepBranches` is
//! about a project on a plain git remote, which this agent has no place for, so it is refused as the TS agent refuses it
//! for any other project.
use serde_json::Value;

use super::{Command, ack, nack};
use crate::view::AgentView;

pub(super) fn backlog_link(cmd: &Command, view: &AgentView) -> Value {
    match view.backlog_link.start() {
        Ok(()) => ack(&cmd.id),
        Err(why) => nack(&cmd.id, why),
    }
}

/// Why lanes aren't asked of the site.
const LANES: &str = "lanes are the computer's, shared by all its projects: set them at the computer (its Settings page or RETASKABLE_LANES).";

/// Why a `settings_request` names a field that is the project's, or `None` (`projectFieldsRefusal`): the hub applies the
/// same rule before it sends one.
fn project_fields_refusal(fields: &[&str]) -> Option<String> {
    let lanes = fields.contains(&"lanes").then_some(LANES);
    let rest: Vec<&str> = fields.iter().copied().filter(|f| *f != "lanes").collect();
    if rest.is_empty() {
        return lanes.map(str::to_string);
    }
    let one = rest.len() == 1;
    Some(format!(
        "{} {} the project's, kept on retaskable.com: change {} in Project settings there.{}",
        rest.join(", "),
        if one { "is" } else { "are" },
        if one { "it" } else { "them" },
        lanes.map(|l| format!(" The {l}")).unwrap_or_default()
    ))
}

pub(super) fn settings_request(cmd: &Command) -> Value {
    let fields: Vec<&str> = cmd
        .args
        .as_object()
        .map(|o| o.keys().map(String::as_str).filter(|k| *k != "keepBranches").collect())
        .unwrap_or_default();
    if let Some(why) = project_fields_refusal(&fields) {
        return nack(&cmd.id, why);
    }
    match cmd.args.as_object() {
        Some(o) if !o.is_empty() => nack(
            &cmd.id,
            "Only a project on a plain git remote can keep its changes' branches on this computer.".into(),
        ),
        _ => nack(&cmd.id, "Name a change.".into()),
    }
}
