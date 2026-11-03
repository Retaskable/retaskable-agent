//! Commands from retaskable.com (cloud.md section 6, `agent/src/commands.ts`): checked in the
//! order section 6.2 gives (replay, expiry, Setup done, the command's own), then run and
//! answered with `ack` or `nack`. Ported one command at a time; `connector::receive` sends every
//! other command here too, so a name this agent doesn't run yet still gets the right nack and the
//! same replay/expiry checks as one it does.
//!
//! So far this runs **Continue**, **Start over** (`retry`) and **Skip** (`skip`) (queued for the orchestrator loop, in `claim` mode only: the loop moves the item,
//! `orchestrator::driver::item_commands`), **Restart**, **Pause**, **Resume** (`{ computer: true }` only: the
//! project-wide pause is the shared backlog's, answered by retaskable.com itself without
//! reaching a computer, agent/09-web-ui.md section 9.3) and **Shoot** (fire-and-forget: the ack
//! comes once `galleries::shoot` has started on its own thread, tray.md section 5.3.3) and
//! **Play main** / **Stop** (`{ lane: 0 }`, [`crate::play`]; a lane from 1 is queued for the loop,
//! `orchestrator::driver::lane_play`), **ask** (a chat message, queued for the loop, `orchestrator::driver::asks`), **draft_files** (answered live from the draft's worktree), **draft_edit** ([`crate::drafts::actions`]) and **draft_approve** / **merge_pr** / **draft_discard** `{ draftId }` (**Approve**, **Merge into main** and **Discard** on a spec draft, queued for the loop, `orchestrator::driver::land`). Every command checked, run or refused is written to
//! `.retaskable/remote.log` ([`audit`]).

use std::collections::HashMap;
use std::path::Path;

use serde_json::{Map, Value, json};

use crate::galleries;
use crate::paths;
use crate::telemetry::events::{iso_utc_millis, now_ms};
use crate::view::{AgentView, Ask, DraftCommand, Exit, ItemCommand};

/// A command lives 60 s from when it was sent (cloud.md section 6.2), whatever `expires_at` says.
const COMMAND_TTL_MS: i64 = 60_000;
/// Command ids are remembered this long, so a replay does nothing (cloud.md section 6.2).
const REPLAY_MS: i64 = 10 * 60_000;
/// What a command that only writes the shared backlog says if it reaches a computer (`agent/src/commands.ts`'s
/// `SITE_ANSWERS`): retaskable.com answers `pause`/`resume` itself unless they name `{ computer: true }`.
const SITE_ANSWERS: &str = "retaskable.com answers this one itself; it doesn't take a computer.";
/// Commands Setup exempts (cloud.md section 6.2's step 3); none of this agent's few ported
/// commands are among them, so each still needs Setup done first.
const BEFORE_SETUP: &[&str] = &["link_project", "setup_check", "install_tool", "tool_sign_in"];

/// A `command` message's fields, as the hub sends them (cloud.md section 5.3): `issued_at` and
/// `expires_at` are epoch milliseconds.
pub struct Command {
    pub id: String,
    pub name: String,
    pub args: Value,
    pub issued_by: String,
    pub issued_at: i64,
    pub expires_at: i64,
}

/// Command ids seen in the last [`REPLAY_MS`], with when (cloud.md section 6.2's replay check).
/// One per project connection, kept across reconnects (`agent/src/commands.ts`'s `Commands.seen`
/// is one per project too, made when its connection is).
#[derive(Default)]
pub struct Seen(HashMap<String, i64>);

impl Seen {
    /// True when `id` was already seen within [`REPLAY_MS`]; remembers it regardless, so a
    /// command this agent doesn't run yet still counts as seen, and forgets ids older than that.
    fn check(&mut self, id: &str, now: i64) -> bool {
        self.0.retain(|_, at| now - *at <= REPLAY_MS);
        let replay = self.0.contains_key(id);
        self.0.insert(id.to_string(), now);
        replay
    }
}

/// Checked in order (cloud.md section 6.2): replay, expiry, then Setup done. The first failing
/// check is the nack reason; `None` to run the command (or nack it for its own reason, in [`run`]).
fn refusal(cmd: &Command, project: &str, seen: &mut Seen, now: i64) -> Option<String> {
    if seen.check(&cmd.id, now) {
        return Some("Already received: a command runs once.".to_string());
    }
    if now > cmd.expires_at.min(cmd.issued_at.saturating_add(COMMAND_TTL_MS)) {
        return Some("Expired: it was sent more than 60 s ago (or this machine's clock is off).".to_string());
    }
    if project.is_empty() && !BEFORE_SETUP.contains(&cmd.name.as_str()) {
        return Some("Finish setup first.".to_string());
    }
    None
}

/// Runs `cmd` against `project` (empty before Setup) if every check passes, and answers it.
/// `seen` and `view.paused` persist for the project's connection, across reconnects. `data_dir` is
/// the agent's own data directory, where Shoot writes a gallery's converted pictures.
pub fn run(cmd: &Command, project: &str, seen: &mut Seen, view: &AgentView, exit: &Exit, data_dir: &Path) -> Value {
    if let Some(reason) = refusal(cmd, project, seen, now_ms()) {
        return nack(&cmd.id, reason);
    }
    let computer = cmd.args.get("computer").and_then(Value::as_bool) == Some(true);
    match cmd.name.as_str() {
        "restart" => {
            // The first request's `why` stays; the exit is shared with the settings page's button.
            view.restart.ask(crate::restart::Why {
                at: now_ms(),
                by: cmd.issued_by.clone(),
                reason: "asked from retaskable.com".into(),
            });
            exit(75);
            ack(&cmd.id)
        }
        "pause" if computer => {
            pause_computer(view, project, &format!("retaskable.com by {}", cmd.issued_by));
            ack(&cmd.id)
        }
        "resume" if computer => {
            resume_computer(view, project);
            ack(&cmd.id)
        }
        "pause" | "resume" => nack(&cmd.id, SITE_ANSWERS.to_string()),
        "shoot" => shoot(cmd, project, view, data_dir),
        "continue" => item_command(cmd, project, view, data_dir, false),
        "retry" => item_command(cmd, project, view, data_dir, true),
        "skip" => skip(cmd, project, view),
        "play" | "stop" => play(cmd, project, view),
        "draft_files" => draft_files(cmd, project, view),
        "draft_edit" => draft_change(cmd, project, view),
        "draft_approve" | "merge_pr" | "draft_discard" => draft_press(cmd, project, view),
        "ask" => ask(cmd, project, view, data_dir),
        other => nack(&cmd.id, format!("Not a command this agent takes yet: {other}")),
    }
}

/// Pictures a message may carry (`shared/sentPictures.ts`, tray/05.3 section 5.3.4).
const MAX_PICTURES: usize = 4;
const MAX_PICTURE_BYTES: usize = 1 << 20;
const MAX_PICTURES_BYTES: usize = 4 << 20;

/// Checks `args.pictures` (`[{ type, data }]`, base64) and saves them under `<data_dir>/attachments/` as
/// `<uuid>.<ext>`. All are checked before any is written; one that fails fails them all. The paths, in order.
fn save_pictures(args: &Value, data_dir: &Path) -> Result<Vec<std::path::PathBuf>, String> {
    use base64::Engine;
    let list = match args.get("pictures") {
        None | Some(Value::Null) => return Ok(vec![]),
        Some(Value::Array(list)) => list,
        Some(_) => return Err("pictures must be a list.".into()),
    };
    if list.len() > MAX_PICTURES {
        return Err(format!("At most {MAX_PICTURES} pictures to a message."));
    }
    let mut decoded = Vec::new();
    let mut total = 0;
    for (i, p) in list.iter().enumerate() {
        let ext = match p.get("type").and_then(Value::as_str) {
            Some("image/png") => "png",
            Some("image/jpeg") => "jpg",
            Some("image/gif") => "gif",
            Some("image/webp") => "webp",
            _ => return Err(format!("Picture {} isn't a PNG, JPEG, GIF or WebP.", i + 1)),
        };
        let bytes = (p.get("data").and_then(Value::as_str))
            .and_then(|d| base64::engine::general_purpose::STANDARD.decode(d).ok())
            .filter(|b| !b.is_empty())
            .ok_or_else(|| format!("Picture {} isn't valid base64.", i + 1))?;
        if bytes.len() > MAX_PICTURE_BYTES {
            return Err(format!("Picture {} is over 1 MiB.", i + 1));
        }
        total += bytes.len();
        decoded.push((ext, bytes));
    }
    if total > MAX_PICTURES_BYTES {
        return Err("The pictures are over 4 MiB in all.".into());
    }
    let dir = data_dir.join("attachments");
    std::fs::create_dir_all(&dir).map_err(|e| format!("Couldn't save the pictures: {e}"))?;
    let mut paths = Vec::new();
    for (ext, bytes) in decoded {
        let path = dir.join(format!("{}.{ext}", crate::implementer::new_session_id()));
        std::fs::write(&path, bytes).map_err(|e| format!("Couldn't save the pictures: {e}"))?;
        paths.push(path);
    }
    Ok(paths)
}

/// **Continue** (`continue { id, notes?, pictures? }`) and **Start over** (`retry`, same arguments): saves the
/// pictures, adds their paths after the notes, and queues the command for the orchestrator loop
/// (`orchestrator::driver::item_commands`), which carries it out on its next pass. Refused outside `claim` mode,
/// the only mode that runs sessions.
fn item_command(cmd: &Command, project: &str, view: &AgentView, data_dir: &Path, start_over: bool) -> Value {
    let id = cmd.args.get("id").and_then(Value::as_str).map(str::trim).unwrap_or("");
    if id.is_empty() {
        let what = if start_over { "Start over" } else { "Continue" };
        return nack(&cmd.id, format!("{what} needs the item's id."));
    }
    if let Err(reason) = claim_check(view) {
        return nack(&cmd.id, reason);
    }
    let pictures = match save_pictures(&cmd.args, data_dir) {
        Ok(p) => p,
        Err(reason) => return nack(&cmd.id, reason),
    };
    let mut notes = cmd
        .args
        .get("notes")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    for path in &pictures {
        notes.push_str(if notes.is_empty() { "Picture: " } else { "\nPicture: " });
        notes.push_str(&path.display().to_string());
    }
    queue_item(view, project, id, (!notes.is_empty()).then_some(notes), start_over);
    ack(&cmd.id)
}

/// "Attached images…" block of an ask's saved pictures (`attachments.ts`'s trailer), empty with none.
fn images_block(paths: &[std::path::PathBuf]) -> String {
    if paths.is_empty() {
        return String::new();
    }
    let list: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
    format!(
        "Attached images (look at each with your Read tool before acting on this):\n{}",
        list.join("\n")
    )
}

/// `ask { askId, text, replyTo?, about?, images? }` (cloud.md section 6; agent/06.6-ask-claude.md): a message in the
/// chat, saved with its pictures as Continue's are and queued for the orchestrator loop, which answers it with
/// `ask_report`. Acked once queued; refused outside `claim` mode.
fn ask(cmd: &Command, project: &str, view: &AgentView, data_dir: &Path) -> Value {
    let text_arg = |k: &str| match cmd.args.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.trim().to_string()).filter(|s| !s.is_empty())),
        Some(_) => Err(format!("ask: {k} must be text.")),
    };
    let args = (|| {
        Ok((
            text_arg("askId")?,
            text_arg("text")?,
            text_arg("replyTo")?,
            text_arg("about")?,
        ))
    })();
    let (id, text, reply_to, about) = match args {
        Ok((Some(id), Some(text), reply_to, about)) => (id, text, reply_to, about),
        Ok((None, ..)) => return nack(&cmd.id, "ask needs the message's askId.".into()),
        Ok(_) => return nack(&cmd.id, "ask needs the message's text.".into()),
        Err(reason) => return nack(&cmd.id, reason),
    };
    if let Err(reason) = claim_check(view) {
        return nack(&cmd.id, reason);
    }
    let pictures = json!({ "pictures": cmd.args.get("images").cloned().unwrap_or(Value::Null) });
    let saved = match save_pictures(&pictures, data_dir) {
        Ok(p) => p,
        Err(reason) => return nack(&cmd.id, reason),
    };
    let images_block = images_block(&saved);
    let text = if images_block.is_empty() {
        text
    } else {
        format!("{text}\n\n{images_block}")
    };
    view.item_commands.push_ask(Ask {
        project: project.to_string(),
        id,
        text,
        reply_to,
        about,
        images_block,
    });
    ack(&cmd.id)
}

/// Item commands and lane play/stop only run where the orchestrator claims work.
fn claim_check(view: &AgentView) -> Result<(), String> {
    if view.item_commands.claim() {
        Ok(())
    } else {
        Err("This computer isn't running work (the orchestrator isn't in claim mode).".into())
    }
}

fn queue_item(view: &AgentView, project: &str, id: &str, notes: Option<String>, start_over: bool) {
    let (project, item) = (project.to_string(), id.to_string());
    view.item_commands.push(if start_over {
        ItemCommand::StartOver { project, item, notes }
    } else {
        ItemCommand::Continue { project, item, notes }
    });
}

/// **Pause** of this computer's project (`{ computer: true }`); `from` says who, in the reason shown.
pub(crate) fn pause_computer(view: &AgentView, project: &str, from: &str) {
    view.paused.set(project, Some(format!("Paused from {from}")));
}

/// **Resume** of this computer's project.
pub(crate) fn resume_computer(view: &AgentView, project: &str) {
    view.paused.set(project, None);
}

/// **Start over** with no notes or pictures: what the command and the assistant's `retry` both run. `Err` is the
/// nack reason.
pub(crate) fn retry_item(view: &AgentView, project: &str, id: &str) -> Result<(), String> {
    let id = id.trim();
    if id.is_empty() {
        return Err("Start over needs the item's id.".into());
    }
    claim_check(view)?;
    queue_item(view, project, id, None, true);
    Ok(())
}

/// **Skip**: what the command and the assistant's `skip` both run. `Err` is the nack reason.
pub(crate) fn skip_item(view: &AgentView, project: &str, id: &str) -> Result<(), String> {
    let id = id.trim();
    if id.is_empty() {
        return Err("Skip needs the item's id.".into());
    }
    claim_check(view)?;
    view.item_commands.push(ItemCommand::Skip {
        project: project.to_string(),
        item: id.to_string(),
    });
    Ok(())
}

/// **Skip** (`skip { id }`): queued for the orchestrator loop, which lets the item go and skips it in the store
/// (`orchestrator::driver::item_commands`). Refused outside `claim` mode, like Continue.
fn skip(cmd: &Command, project: &str, view: &AgentView) -> Value {
    let id = cmd.args.get("id").and_then(Value::as_str).unwrap_or("");
    answer(cmd, skip_item(view, project, id))
}

/// Finds `cmd`'s gallery by its `gallery` slug (`GalleryConfig::slug`) among `project`'s own, runs
/// [`galleries::shoot_refusal`]'s checks, then spawns [`galleries::shoot`] on its own thread and
/// acks at once (tray.md section 5.3.3: "the ack comes once the shoot has started").
fn shoot(cmd: &Command, project: &str, view: &AgentView, data_dir: &Path) -> Value {
    let slug = cmd.args.get("gallery").and_then(Value::as_str).unwrap_or("");
    let not_found = || nack(&cmd.id, format!("No gallery called {slug}."));
    let Some(p) = view.projects.iter().find(|p| p.id == project) else {
        return not_found();
    };
    let Some(gallery) = p.galleries.iter().find(|g| g.slug() == slug) else {
        return not_found();
    };
    if let Some(reason) = galleries::shoot_refusal(project, gallery) {
        return nack(&cmd.id, reason);
    }
    let project = project.to_string();
    let slug = slug.to_string();
    let gallery = gallery.clone();
    let repo_path = p.repo_path.clone();
    let worktrees_dir = p.worktrees_dir.clone();
    let base_branch = p.base_branch.clone();
    let dest = paths::gallery_dir(data_dir, &project, &slug);
    std::thread::spawn(move || {
        let _ = galleries::shoot(
            &project,
            &slug,
            &gallery,
            &repo_path,
            &worktrees_dir,
            &base_branch,
            &dest,
        );
    });
    ack(&cmd.id)
}

/// `play { lane }` / `stop { lane }`.
fn play(cmd: &Command, project: &str, view: &AgentView) -> Value {
    let lane = cmd.args.get("lane").and_then(Value::as_i64);
    answer(cmd, play_lane(view, project, cmd.name == "play", lane))
}

/// `play` (or `stop`) of a lane: lane 0 is Play main ([`crate::play`], which waits up to `play::STOP_LIMIT` to stop).
/// A lane from 1 is queued for the orchestrator loop, which owns the lanes and decides
/// (`orchestrator::driver::lane_play`). `Err` is the nack reason.
pub(crate) fn play_lane(view: &AgentView, project: &str, play: bool, lane: Option<i64>) -> Result<(), String> {
    let name = if play { "play" } else { "stop" };
    match lane {
        Some(0) => {}
        Some(k) if k > 0 && k <= i64::from(view.lanes) => {
            claim_check(view)?;
            let Some(p) = view.projects.iter().find(|p| p.id == project) else {
                return Err("Finish setup first.".to_string());
            };
            let (project, lane) = (project.to_string(), k as u32);
            if play {
                if p.play_command.trim().is_empty() {
                    return Err("No play command set for this project.".to_string());
                }
                view.item_commands.push(ItemCommand::Play { project, lane });
            } else {
                view.item_commands.push(ItemCommand::Stop { project, lane });
            }
            return Ok(());
        }
        Some(k) if k > 0 => return Err(format!("This computer has no lane {k}.")),
        _ => return Err(format!("{name} needs a lane.")),
    }
    let Some(p) = view.projects.iter().find(|p| p.id == project) else {
        return Err("Finish setup first.".to_string());
    };
    if play {
        crate::play::start(project, p)
    } else {
        crate::play::stop(project)
    }
}

/// `ack`, or `nack` with the reason.
fn answer(cmd: &Command, done: Result<(), String>) -> Value {
    match done {
        Ok(()) => ack(&cmd.id),
        Err(reason) => nack(&cmd.id, reason),
    }
}

/// `draft_files { id }`: the draft's changed files and patches, listed live from its worktree
/// (`<worktreesDir>/draft-<id>`) and answered in the ack's `answer.files`; nothing is stored
/// (06.5-spec-drafts.md). A draft whose worktree is gone is nacked.
fn draft_files(cmd: &Command, project: &str, view: &AgentView) -> Value {
    let id = cmd.args.get("id").and_then(Value::as_str).map(str::trim).unwrap_or("");
    if id.is_empty() || id.contains(['/', '\\']) || id.contains("..") {
        return nack(&cmd.id, "draft_files needs the draft's id.".into());
    }
    let Some(p) = view.projects.iter().find(|p| p.id == project) else {
        return nack(&cmd.id, "Finish setup first.".to_string());
    };
    let dir = crate::draft_worktree::path(&p.worktrees_dir, id);
    if !dir.is_dir() {
        return nack(
            &cmd.id,
            format!("No worktree for draft {id} on this computer (it may be finished or removed)."),
        );
    }
    match crate::drafts::files::draft_files(&dir, &p.base_branch) {
        Ok(files) => json!({ "type": "ack", "id": cmd.id, "answer": { "files": files } }),
        Err(e) => nack(&cmd.id, format!("Couldn't list draft {id}'s files: {e:#}")),
    }
}

/// `draft_edit { id, path, content }` writes the person's file into the draft's worktree, commits and pushes it to
/// the draft's PR branch, and answers with the files listed again (`answer.files`). Nothing on retaskable.com sends
/// it yet. `branch` may name the draft's branch; without it the checkout's `retaskable/spec-<id>-…` is used.
fn draft_change(cmd: &Command, project: &str, view: &AgentView) -> Value {
    use crate::drafts::actions::{Where, branch_of, edit};
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
    match edit(&w, id, &branch, file, content) {
        Ok(files) => json!({ "type": "ack", "id": cmd.id, "answer": { "files": files } }),
        Err(e) => nack(&cmd.id, format!("Couldn't edit draft {id}: {e:#}")),
    }
}

/// `draft_approve { draftId }` (**Approve and merge**), `merge_pr { draftId }` (**Merge into main**) or
/// `draft_discard { draftId }` (**Discard**): queued for the
/// loop, which takes the draft and lands it. A `merge_pr` for an item or the team branch isn't this agent's yet.
fn draft_press(cmd: &Command, project: &str, view: &AgentView) -> Value {
    let draft = cmd
        .args
        .get("draftId")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if cmd.name == "merge_pr" && draft.is_empty() {
        return nack(&cmd.id, format!("Not a command this agent takes yet: {}", cmd.name));
    }
    if draft.is_empty() {
        return nack(&cmd.id, format!("{} needs the draft's id.", cmd.name));
    }
    if !view.item_commands.claim() {
        return nack(
            &cmd.id,
            "This computer isn't running work (the orchestrator isn't in claim mode).".into(),
        );
    }
    let (project, draft) = (project.to_string(), draft.to_string());
    view.item_commands.push_draft(match cmd.name.as_str() {
        "merge_pr" => DraftCommand::Merge { project, draft },
        "draft_discard" => DraftCommand::Discard { project, draft },
        _ => DraftCommand::Approve { project, draft },
    });
    ack(&cmd.id)
}

fn ack(id: &str) -> Value {
    json!({ "type": "ack", "id": id })
}

fn nack(id: &str, reason: String) -> Value {
    json!({ "type": "nack", "id": id, "reason": reason })
}

/// A command's args for the log: pictures (or an ask's images) as `{ count, bytes }` only, never their data (tray/05.3 section 5.3.4).
fn audit_args(args: &Value) -> Value {
    let mut args = args.clone();
    for key in ["pictures", "images"] {
        if let Some(Value::Array(list)) = args.get(key) {
            let bytes: usize = (list.iter())
                .filter_map(|p| p.get("data").and_then(Value::as_str))
                .map(str::len)
                .sum();
            args[key] = json!({ "count": list.len(), "bytes": bytes });
        }
    }
    args
}

/// `.retaskable/remote.log` (cloud.md section 6.3): one JSON line per command received, refused
/// ones and replays included, never a `read_file`'s content — moot so far, since no read command
/// is ported yet. `answerBytes` and `auditArgs`'s release_edit/ci_link/picture-count sanitising
/// (`agent/src/commands.ts`) aren't ported either, for the same reason: none of those commands are.
/// A write that fails is dropped silently, the same as the event log's.
pub fn audit(dir: &Path, cmd: &Command, answer: &Value, cloud: &str) {
    let outcome = answer.get("type").and_then(Value::as_str).unwrap_or("nack");
    let mut entry = Map::new();
    entry.insert("at".into(), json!(iso_utc_millis(now_ms())));
    entry.insert("id".into(), json!(cmd.id));
    entry.insert("name".into(), json!(cmd.name));
    entry.insert("issuedBy".into(), json!(cmd.issued_by));
    entry.insert("args".into(), audit_args(&cmd.args));
    entry.insert("outcome".into(), json!(outcome));
    if outcome == "nack" {
        entry.insert("reason".into(), answer.get("reason").cloned().unwrap_or(Value::Null));
    }
    if let Some(answer) = answer.get("answer") {
        entry.insert("answerBytes".into(), json!(answer.to_string().len()));
    }
    entry.insert("cloud".into(), json!(cloud));
    let line = format!("{}\n", Value::Object(entry));
    let _ = std::fs::create_dir_all(dir).and_then(|()| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("remote.log"))
            .and_then(|mut f| {
                use std::io::Write;
                f.write_all(line.as_bytes())
            })
    });
}

#[cfg(test)]
mod tests;
