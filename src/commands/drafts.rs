//! The commands on a spec draft: `draft_edit`, the presses (`draft_approve`, `merge_pr`, `draft_discard`) and the
//! design review's (`draft_skip_design`, `board_design_write`). A draft's files are no longer a pushed read
//! (`draft_files`): they are a fact the agent keeps in the store (`kept_facts`), and `draft_edit` answers with them.
use serde_json::{Value, json};

use super::{Command, PENDING, ack, acts_check, nack};
use crate::view::{AgentView, DraftCommand};

/// `draft_edit { id, path, content }` writes the person's file into the draft's worktree, commits and pushes it to
/// the draft's PR branch, and answers with the files listed again (`answer.files`). Nothing on retaskable.com sends
/// it yet. `branch` may name the draft's branch; without it the checkout's `retaskable/spec-<id>-…` is used.
pub(super) fn edit(cmd: &Command, project: &str, view: &AgentView) -> Value {
    use crate::drafts::actions::{Where, branch_of, edit as edit_file};
    let arg = |k: &str| cmd.args.get(k).and_then(Value::as_str);
    let id = arg("id").map(str::trim).unwrap_or("");
    if id.is_empty() || id.contains(['/', '\\']) || id.contains("..") {
        return nack(&cmd.id, format!("{} needs the draft's id.", cmd.name));
    }
    let Some(p) = view.projects.iter().find(|p| p.id == project) else {
        return nack(&cmd.id, "Finish setup first.".to_string());
    };
    let w = Where {
        repo_path: &p.repo_path,
        worktrees_dir: &p.worktrees_dir,
        base_branch: &p.base_branch,
    };
    let branch = match branch_of(&w, id, arg("branch")) {
        Ok(b) => b,
        Err(e) => return nack(&cmd.id, format!("{e:#}")),
    };
    let (Some(file), Some(content)) = (arg("path"), arg("content")) else {
        return nack(&cmd.id, "draft_edit needs the file's path and content.".into());
    };
    match edit_file(&w, id, &branch, file, content) {
        Ok(files) => json!({ "type": "ack", "id": cmd.id, "result": { "files": files } }),
        Err(e) => nack(&cmd.id, format!("Couldn't edit draft {id}: {e:#}")),
    }
}

/// `draft_approve { draftId }` (**Approve and merge**), `merge_pr { draftId }` (**Merge into main**) or
/// `draft_discard { draftId }` (**Discard**): queued for the
/// loop, which takes the draft and lands it; a `merge_pr` is squashed, as a person's merge is in the TS agent. A
/// `merge_pr` for an item or the team branch is [`super::item_actions::merge_pr`]'s.
pub(super) fn press(cmd: &Command, project: &str, view: &AgentView) -> Value {
    let draft = cmd
        .args
        .get("draftId")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if draft.is_empty() {
        return nack(&cmd.id, format!("{} needs the draft's id.", cmd.name));
    }
    if let Err(reason) = acts_check(view) {
        return nack(&cmd.id, reason);
    }
    let (project, draft) = (project.to_string(), draft.to_string());
    view.item_commands.push_draft(match cmd.name.as_str() {
        "merge_pr" => DraftCommand::Merge { project, draft },
        "draft_discard" => DraftCommand::Discard { project, draft },
        _ => DraftCommand::Approve { project, draft },
    });
    ack(&cmd.id)
}

/// `draft_skip_design { draftId }` (**Skip the design**) and `board_design_write { draftId, note? }` (**Try again**, and
/// **Change it…** with its `note`, at most 2000 characters): queued for the loop, which does the press
/// (`orchestrator::driver::design_press`) and answers the command when it has: `ack`, or `nack` with the reason (a plan
/// not waiting for approval, one with no design to review, why the designs can't be drawn). Both run in `claim` and `runs`
/// mode, paused or not, as Approve does.
pub(super) fn design(cmd: &Command, project: &str, view: &AgentView) -> Value {
    let draft = cmd
        .args
        .get("draftId")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if draft.is_empty() || draft.contains(['/', '\\']) || draft.contains("..") {
        return nack(&cmd.id, format!("{} needs the draft's id.", cmd.name));
    }
    if let Err(reason) = acts_check(view) {
        return nack(&cmd.id, reason);
    }
    let (project, draft, command_id) = (project.to_string(), draft.to_string(), cmd.id.clone());
    view.item_commands.push_draft(if cmd.name == "draft_skip_design" {
        DraftCommand::SkipDesign {
            project,
            draft,
            command_id,
        }
    } else {
        let note = cmd
            .args
            .get("note")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(|n| crate::text::truncate(n, 2000));
        DraftCommand::DrawDesign {
            project,
            draft,
            note,
            command_id,
        }
    });
    json!({ "type": PENDING, "id": cmd.id })
}
