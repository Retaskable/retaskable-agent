//! Commands from retaskable.com (cloud.md section 6, `agent/src/commands.ts`): checked in the
//! order section 6.2 gives (replay, expiry, Setup done, the command's own), then run and
//! answered with `ack` or `nack`. Ported one command at a time; `connector::receive` sends every
//! other command here too, so a name this agent doesn't run yet still gets the right nack and the
//! same replay/expiry checks as one it does.
//!
//! So far this runs **idea_accept** (queued for the loop, answered later), **Continue**, **Start over** (`retry`) and **Skip** (`skip`) (queued for the orchestrator loop, in `claim` mode only: the loop moves the item,
//! `orchestrator::driver::item_commands`), **Restart**, **Pause**, **Resume** (`{ computer: true }` only: the
//! project-wide pause is the shared backlog's, answered by retaskable.com itself without
//! reaching a computer, agent/09-web-ui.md section 9.3) and
//! **Play main** / **Stop** (`{ lane: 0 }`, [`crate::play`]; **Restart** is `restart_play { lane: 0 }`, and `stop_play` is another name for **Stop**; a lane from 1 is queued for the loop, `restart_play` too,
//! `orchestrator::driver::lane_play`), **ask** (a chat message, queued for the loop, `orchestrator::driver::asks`), **draft_files** (answered live from the draft's worktree), **draft_edit** ([`crate::drafts::actions`]) and **draft_approve** / **merge_pr** / **draft_discard** `{ draftId }` (**Approve**, **Merge into main** and **Discard** on a spec draft, queued for the loop, `orchestrator::driver::land`). Every command checked, run or refused is written to
//! `.retaskable/remote.log` ([`audit`]).

use crate::text::is_blank;
use std::collections::HashMap;
use std::path::Path;

use serde_json::{Map, Value, json};

use crate::telemetry::events::{iso_utc_millis, now_ms};
use crate::view::{AgentView, AskCommand, DraftCommand, Exit, ItemCommand};

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
/// the agent's own data directory.
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
        "pause" | "resume" | "idea_later" | "idea_never" => nack(&cmd.id, SITE_ANSWERS.to_string()),
        "idea_accept" => idea_accept(cmd, project, view),
        "setup_check" => setup_check(cmd, project, view),
        "continue" => item_command(cmd, project, view, data_dir, false),
        "retry" => item_command(cmd, project, view, data_dir, true),
        "skip" => skip(cmd, project, view),
        "playtest" => playtest(cmd, project, view, data_dir),
        "play" | "stop" | "stop_play" | "restart_play" => play(cmd, project, view),
        "ask" => ask(cmd, project, view, data_dir),
        "agent_settings" => agent_settings(cmd, view),
        "draft_files" => draft_files(cmd, project, view),
        "approve_commands" => approve_commands(cmd, project, view),
        "draft_edit" => draft_change(cmd, project, view),
        "draft_approve" | "merge_pr" | "draft_discard" => draft_press(cmd, project, view),
        other => nack(&cmd.id, format!("Not a command this agent takes yet: {other}")),
    }
}

/// **agent_settings** (agent-settings.md section 2.1): saved at once, with no one at the computer to allow it. The hub
/// checked who sent it; this re-checks nothing about the sender. A key outside the owner's five is refused, and a
/// refused change writes nothing. A save fires the new-hello signal so every project's hub sees the new values.
// There is no session pool yet: when there is, `pool.configure` goes here, after the save.
fn agent_settings(cmd: &Command, view: &AgentView) -> Value {
    let change: crate::site_settings::AgentSettings = match serde_json::from_value(cmd.args.clone()) {
        Ok(c) => c,
        Err(_) => {
            return nack(
                &cmd.id,
                "Not a command this agent takes: agent_settings with those settings".into(),
            );
        }
    };
    let Some(dir) = view.settings.dir() else {
        return nack(&cmd.id, "This computer's settings can't be found.".into());
    };
    match crate::site_settings::save_agent_settings(dir, &change) {
        Ok(()) => {
            view.settings.fire();
            ack(&cmd.id)
        }
        Err(e) => nack(&cmd.id, e.to_string()),
    }
}

/// Pictures a message may carry (`shared/sentPictures.ts`, tray/05.3 section 5.3.4).
const MAX_PICTURES: usize = 4;
const MAX_PICTURE_BYTES: usize = 1 << 20;
const MAX_PICTURES_BYTES: usize = 4 << 20;

/// Checks `args.pictures` (`[{ type, data }]`, base64) and saves them under `<data_dir>/attachments/` as
/// `<uuid>.<ext>`. All are checked before any is written; one that fails fails them all. The paths, in order.
fn save_pictures(args: &Value, key: &str, data_dir: &Path) -> Result<Vec<std::path::PathBuf>, String> {
    use base64::Engine;
    let list = match args.get(key) {
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
    let notes = match notes_with_pictures(&cmd.args, data_dir) {
        Ok(n) => n,
        Err(reason) => return nack(&cmd.id, reason),
    };
    queue_item(view, project, id, notes, start_over);
    ack(&cmd.id)
}

/// The notes, trimmed, with the paths of the saved `pictures` added after them; `None` when both are empty.
fn notes_with_pictures(args: &Value, data_dir: &Path) -> Result<Option<String>, String> {
    let pictures = save_pictures(args, "pictures", data_dir)?;
    let mut notes = args
        .get("notes")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    for path in &pictures {
        notes.push_str(if notes.is_empty() { "Picture: " } else { "\nPicture: " });
        notes.push_str(&path.display().to_string());
    }
    Ok((!notes.is_empty()).then_some(notes))
}

/// **playtest** (`playtest { itemId (or id), mergedAt?, result: "pass" | "fail", notes?, pictures? }`), behind the `playtest-answer-command`
/// flag: a person's answer to a playtest, queued for the loop. `itemId` is the item's id, as in TypeScript; `id` is also taken. Refused
/// outside `claim` mode, and for an item the loop holds in a state other than `awaiting_playtest`.
fn playtest(cmd: &Command, project: &str, view: &AgentView, data_dir: &Path) -> Value {
    let text = |k: &str| {
        cmd.args
            .get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };
    let id = text("itemId").or_else(|| text("id")).unwrap_or("");
    if id.is_empty() {
        return nack(&cmd.id, "A playtest answer needs the item's id.".into());
    }
    let passed = match cmd.args.get("result").and_then(Value::as_str) {
        Some("pass") => true,
        Some("fail") => false,
        _ => return nack(&cmd.id, "A playtest answer's result must be pass or fail.".into()),
    };
    if let Err(reason) = claim_check(view) {
        return nack(&cmd.id, reason);
    }
    let held = view
        .live
        .snapshot()
        .projects
        .get(project)
        .and_then(|p| p.items.iter().find(|i| i.id == id).map(|i| i.state.clone()));
    if let Some(state) = held.filter(|s| s != "awaiting_playtest") {
        return nack(&cmd.id, format!("{id} isn't waiting for a playtest (it is {state})."));
    }
    let notes = match notes_with_pictures(&cmd.args, data_dir) {
        Ok(n) => n,
        Err(reason) => return nack(&cmd.id, reason),
    };
    view.item_commands.push(ItemCommand::Playtest {
        project: project.to_string(),
        id: id.to_string(),
        passed,
        notes,
        merged_at: text("mergedAt").map(str::to_string),
    });
    ack(&cmd.id)
}

/// **ask** (`ask { askId, text, replyTo?, about?, images? }`, spec 06.6): a chat message for the assistant. The images
/// are saved like a message's pictures and their paths go in an "Attached images…" block; the ask is queued for the
/// orchestrator loop, which answers it with `ask_report`. Refused outside `claim` mode, the only mode that runs sessions.
fn ask(cmd: &Command, project: &str, view: &AgentView, data_dir: &Path) -> Value {
    let text_of = |key: &str| {
        let v = cmd.args.get(key);
        match v {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => Ok(crate::text::non_blank(s).map(str::to_string)),
            Some(_) => Err(format!("ask's {key} must be text.")),
        }
    };
    let fields = (|| {
        Ok((
            text_of("askId")?,
            text_of("text")?,
            text_of("replyTo")?,
            text_of("about")?,
        ))
    })();
    let (ask_id, text, reply_to, about) = match fields {
        Ok((Some(ask_id), Some(text), reply_to, about)) => (ask_id, text, reply_to, about),
        Ok((None, ..)) => return nack(&cmd.id, "ask needs its askId.".into()),
        Ok(_) => return nack(&cmd.id, "ask needs the message's text.".into()),
        Err(reason) => return nack(&cmd.id, reason),
    };
    if let Err(reason) = claim_check(view) {
        return nack(&cmd.id, reason);
    }
    let images = match save_pictures(&cmd.args, "images", data_dir) {
        Ok(p) => p,
        Err(reason) => return nack(&cmd.id, reason),
    };
    let images_block = if images.is_empty() {
        String::new()
    } else {
        let list: Vec<String> = images.iter().map(|p| format!("- {}", p.display())).collect();
        format!(
            "Attached images (look at each with your Read tool before acting on this):\n{}",
            list.join("\n")
        )
    };
    view.item_commands.push_ask(AskCommand {
        project: project.to_string(),
        ask_id,
        text,
        reply_to,
        about,
        images_block,
    });
    ack(&cmd.id)
}

/// What [`run`] answers a command the loop settles once the store has answered: nothing is sent yet, and the loop sends
/// the `ack` or `nack` later ([`crate::view::ItemCommands::answer`]).
pub const PENDING: &str = "pending";

/// `setup_check {}` (**Check again**, first-run-check.md section 3.1): runs the fixed probes now. They take up to 20 s
/// each, so this answers `pending` and a task sends the `ack` with the checks as its `answer`. The run goes through the
/// runner, so it shares the one-at-a-time lock and a changed result also goes up in `hello`.
fn setup_check(cmd: &Command, project: &str, view: &AgentView) -> Value {
    let no_args = match &cmd.args {
        Value::Null => true,
        Value::Object(o) => o.is_empty(),
        _ => false,
    };
    if !no_args {
        return nack(&cmd.id, "setup_check takes no arguments.".into());
    }
    let (runner, commands) = (view.checks.clone(), view.item_commands.clone());
    let (id, project) = (cmd.id.clone(), project.to_string());
    tokio::spawn(async move {
        let checks = runner.run_now().await;
        commands.answer(&project, json!({ "type": "ack", "id": id, "answer": checks.to_json() }));
    });
    json!({ "type": PENDING, "id": cmd.id })
}

/// `idea_accept { idea }` (**Build this** on an idea): queued for the loop, which reads the
/// idea from the store, queues the draft and answers (`ideas::accept`). Not now and Never are the site's.
fn idea_accept(cmd: &Command, project: &str, view: &AgentView) -> Value {
    let idea = cmd
        .args
        .get("idea")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if idea.is_empty() {
        return nack(&cmd.id, "idea_accept needs the idea's id.".into());
    }
    if let Err(reason) = claim_check(view) {
        return nack(&cmd.id, reason);
    }
    view.item_commands.push_idea(crate::view::IdeaCommand {
        project: project.to_string(),
        command_id: cmd.id.clone(),
        idea: idea.to_string(),
        by: cmd.issued_by.clone(),
    });
    json!({ "type": PENDING, "id": cmd.id })
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

/// `play { lane }` / `stop { lane }` (also `stop_play`) / `restart_play { lane }`.
fn play(cmd: &Command, project: &str, view: &AgentView) -> Value {
    let lane = cmd.args.get("lane").and_then(Value::as_i64);
    let result = if cmd.name == "restart_play" {
        restart_lane(view, project, lane)
    } else {
        play_lane(view, project, cmd.name == "play", lane)
    };
    if lane == Some(0) && result.is_ok() {
        view.live.bump();
    }
    answer(cmd, result)
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
            let Some(p) = allowed_project(view, project) else {
                return Err("Finish setup first.".to_string());
            };
            let (project, lane) = (project.to_string(), k as u32);
            if play {
                if is_blank(&p.play_command) {
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
    let Some(p) = allowed_project(view, project) else {
        return Err("Finish setup first.".to_string());
    };
    if play {
        crate::play::start(project, &p)
    } else {
        crate::play::stop(project)
    }
}

/// `restart_play` of a lane: lane 0 is Play main ([`crate::play::restart`]); a lane from 1 is queued for the loop, checked as `play` is.
fn restart_lane(view: &AgentView, project: &str, lane: Option<i64>) -> Result<(), String> {
    match lane {
        Some(0) => {}
        Some(k) if k > i64::from(view.lanes) => return Err(format!("This computer has no lane {k}.")),
        Some(k) if k > 0 => {
            claim_check(view)?;
            let Some(p) = allowed_project(view, project) else {
                return Err("Finish setup first.".to_string());
            };
            if is_blank(&p.play_command) {
                return Err("No play command set for this project.".to_string());
            }
            view.item_commands.push(ItemCommand::RestartPlay {
                project: project.to_string(),
                lane: k as u32,
            });
            return Ok(());
        }
        _ => return Err("restart_play needs a lane.".to_string()),
    }
    let Some(p) = allowed_project(view, project) else {
        return Err("Finish setup first.".to_string());
    };
    crate::play::restart(project, &p)
}

/// `approve_commands { version }` the owner, freshly signed in on the site, has the
/// waiting commands run from now on. Nacked from anyone else, without a numeric `version`, or when that isn't the
/// version waiting.
fn approve_commands(cmd: &Command, project: &str, view: &AgentView) -> Value {
    let Some(version) = cmd.args.get("version").and_then(Value::as_u64) else {
        return nack(
            &cmd.id,
            "approve_commands needs the version it shows (a number).".into(),
        );
    };
    if view.owner().as_deref() != Some(cmd.issued_by.as_str()) {
        return nack(
            &cmd.id,
            "Only this computer's owner can approve its project's commands.".into(),
        );
    }
    match view.item_commands.gates().approve(project, version) {
        Ok(_) => {
            view.work.wake(project);
            ack(&cmd.id)
        }
        Err(reason) => nack(&cmd.id, reason),
    }
}

/// The project as the connector holds it, with the commands this computer may run.
fn allowed_project(view: &AgentView, project: &str) -> Option<crate::view::ProjectView> {
    let p = view.projects.iter().find(|p| p.id == project)?;
    Some(view.item_commands.allowed_view(p))
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

/// The size entry for a typed value: `{ bytes }`, the UTF-8 length of a string; any other type is `{ bytes: 0 }`, never its value.
fn sized(value: &Value) -> Value {
    json!({ "bytes": value.as_str().map_or(0, str::len) })
}

/// A command's args for the log, the one place that decides what each command may leave in `remote.log`
/// (cloud.md section 6.3). Per command name: text a person typed (`notes`, `text`, a draft file's
/// `content`) as `{ bytes }`; `pictureIds` as a count; `pictures` (for any command) as `{ count, bytes }`, never their data.
/// A command with no entry logs its args unchanged.
// TODO: `read_file`, `release_edit` and `ci_link` aren't commands this agent takes yet; shape each like
// TypeScript's `auditArgs` (`shared/cloudCommands.ts`) when it is ported.
fn audit_args(name: &str, args: &Value) -> Value {
    let mut args = args.clone();
    let typed: &[&str] = match name {
        "continue" | "retry" | "playtest" => &["notes"],
        "ask" => &["text"],
        "draft_edit" => &["content"],
        _ => &[],
    };
    for key in typed {
        if let Some(v) = args.get_mut(*key) {
            *v = sized(v);
        }
    }
    if let Some(Value::Array(ids)) = args.get("pictureIds") {
        args["pictureIds"] = json!(ids.len());
    }
    if let Some(Value::Array(list)) = args.get("pictures") {
        let bytes: usize = (list.iter())
            .filter_map(|p| p.get("data").and_then(Value::as_str))
            .map(str::len)
            .sum();
        args["pictures"] = json!({ "count": list.len(), "bytes": bytes });
    }
    args
}

/// `.retaskable/remote.log` (cloud.md section 6.3): one JSON line per command received, refused
/// ones and replays included, never an answer's content: a read command (`draft_files`) logs
/// `answerBytes` only, so a picture's base64 never reaches the log. The args go through [`audit_args`], which
/// keeps typed text, picture data and ids out of the line.
/// A write that fails is dropped silently, the same as the event log's.
pub fn audit(dir: &Path, cmd: &Command, answer: &Value, cloud: &str) {
    let outcome = answer.get("type").and_then(Value::as_str).unwrap_or("nack");
    let mut entry = Map::new();
    entry.insert("at".into(), json!(iso_utc_millis(now_ms())));
    entry.insert("id".into(), json!(cmd.id));
    entry.insert("name".into(), json!(cmd.name));
    entry.insert("issuedBy".into(), json!(cmd.issued_by));
    entry.insert("args".into(), audit_args(&cmd.name, &cmd.args));
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
