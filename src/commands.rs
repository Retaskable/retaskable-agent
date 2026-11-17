//! Commands from retaskable.com (cloud.md section 6, `agent/src/commands.ts`): checked in the
//! order section 6.2 gives (replay, expiry, Setup done, the command's own), then run and
//! answered with `ack` or `nack`. Ported one command at a time; `connector::receive` sends every
//! other command here too, so a name this agent doesn't run yet still gets the right nack and the
//! same replay/expiry checks as one it does.
//!
//! So far this runs **Continue**, **Start over** (`retry`) and **Skip** (`skip`) (queued for the orchestrator loop, in `claim` mode only: the loop moves the item,
//! `orchestrator::driver::item_commands`), **Restart**, **Pause**, **Resume** (`{ computer: true }` only: the
//! project-wide pause is the shared backlog's, answered by retaskable.com itself without
//! reaching a computer, agent/09-web-ui.md section 9.3) and
//! **Play main** / **Stop** (`{ lane: 0 }`, [`crate::play`]; **Restart** is `restart_play { lane: 0 }`, and `stop_play` is another name for **Stop**; a lane from 1 is queued for the loop, `restart_play` too,
//! `orchestrator::driver::lane_play`), **draft_edit** ([`crate::drafts::actions`]) and **draft_approve** / **merge_pr** / **draft_discard** `{ draftId }` (**Approve**, **Merge into main** and **Discard** on a spec draft, queued for the loop, `orchestrator::driver::land`). Every command checked, run or refused is written to
//! `.retaskable/remote.log` ([`audit`]). The expert commands (`expert_start`, `expert_say`, `expert_stop`, `expert_end`, `expert_helper`, `session_history`) are queued for the
//! loop too (`orchestrator::driver::experts`).

use crate::text::is_blank;
use std::collections::HashMap;
use std::path::Path;

use serde_json::{Map, Value, json};

use crate::telemetry::events::{iso_utc_millis, now_ms};
use crate::view::{AgentView, Exit, ItemCommand};

/// A command lives 60 s from when it was sent (cloud.md section 6.2), whatever `expires_at` says.
const COMMAND_TTL_MS: i64 = 60_000;
/// Command ids are remembered this long, so a replay does nothing (cloud.md section 6.2).
const REPLAY_MS: i64 = 10 * 60_000;
/// What a command that only writes the shared backlog says if it reaches a computer (`agent/src/commands.ts`'s
/// `SITE_ANSWERS`): retaskable.com answers `pause`/`resume` itself unless they name `{ computer: true }`.
const SITE_ANSWERS: &str = "retaskable.com answers this one itself; it doesn't take a computer.";
/// The commands the site writes as intent rows this computer pulls (`INTENT_COMMANDS` in `shared/storeCommands.ts`,
/// `PULLED_COMMANDS` in the TS agent): pushed, they are refused, so one never arrives both ways.
const PULLED: &[&str] = &[
    "draft_spec",
    "idea_accept",
    "take_over",
    "finish_steps",
    "sync_push",
    "team_sync",
    "main_pr",
    "ci_fix",
    // The reads any computer with what they need answers as rows (`INTENT_READ_COMMANDS`, `intents/reads.rs`).
    "read_file",
    "issues_preview",
    "environments",
];
/// What the hub answers itself with a store operation (`STORE_COMMANDS`, `shared/storeCommands.ts`; `release_edit`,
/// `release_clear`, `release_save`, `audiences`, `chore_run` and the flags in the TS agent's `exec`): they reach a
/// computer only from an old hub, and it says so as `SITE_ANSWERS` does.
const SITE_COMMANDS: &[&str] = &[
    "snooze",
    "piece",
    "depends",
    "manual_approval",
    "asked_by",
    "assign",
    "audience",
    "audiences",
    "draft_step",
    "draft_answer",
    "draft_revise",
    "free",
    "remove",
    "chore_run",
    "release_generate",
    "release_edit",
    "release_clear",
    "release_save",
    "flag_set",
    "flag_drop",
    "flag_rollout",
    "flag_link_make",
    "flag_link_revoke",
    "expert_queue_edit",
    "expert_queue_cancel",
];
const PULLED_REFUSAL: &str =
    "That is a request on retaskable.com now: this computer takes it from there, not as a pushed command.";
/// Commands Setup exempts (cloud.md section 6.2's step 3); none of this agent's few ported
/// commands are among them, so each still needs Setup done first.
const BEFORE_SETUP: &[&str] = &[
    "link_project",
    "setup_check",
    "install_tool",
    "tool_sign_in",
    "github_owners",
    "github_repos",
];

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
        name if SITE_COMMANDS.contains(&name) => nack(&cmd.id, SITE_ANSWERS.to_string()),
        // The site writes these as intent rows a computer pulls (`INTENT_COMMANDS`); pushed, they are refused.
        name if PULLED.contains(&name) => nack(&cmd.id, PULLED_REFUSAL.to_string()),
        "setup_check" => setup_check(cmd, project, view),
        "install_tool" => setup_tools::install_tool(cmd, project, view),
        "tool_sign_in" => setup_tools::tool_sign_in(cmd, view),
        "answer" => item_actions::answer(cmd, project, view),
        "unlink_project" => unlink::unlink_project(cmd, project, view),
        "release_publish" | "release_unpublish" => release_publish::release_page(cmd, project, view),
        "continue" => item_command(cmd, project, view, false),
        "retry" => item_command(cmd, project, view, true),
        "skip" => skip(cmd, project, view),
        "playtest" => playtest(cmd, project, view),
        "play" | "stop" | "stop_play" | "restart_play" => play(cmd, project, view),
        "expert_start" | "expert_say" | "expert_stop" | "expert_end" | "expert_helper" | "session_history" => {
            expert::expert(cmd, project, view)
        }
        "claude_sessions" => expert::claude_sessions(cmd, view),
        "link_project" => link::link_project(cmd, project, view),
        "backlog_link" => computer::backlog_link(cmd, view),
        "settings_request" => computer::settings_request(cmd),
        "release_status" => release_status(cmd, project, view),
        "change_files" => nack(
            &cmd.id,
            "This project's changes are on its code host: open them there.".into(),
        ),
        // The Chat Agent takes chat (docs/specs/agent/06.6): the hub sends a Coding Agent an `ask` only from Discord.
        "ask" => nack(
            &cmd.id,
            "Chat is answered by the Chat Agent, not by this computer's coding agent.".into(),
        ),
        "github_owners" | "github_repos" => github::run(cmd, project, view),
        "phone_play" => phone_play::play(cmd, project, view, data_dir),
        "phone_stop" => phone_play::stop(cmd),
        "agent_settings" => agent_settings(cmd, view),
        "approve_commands" => approve_commands(cmd, project, view),
        "draft_edit" => drafts::edit(cmd, project, view),
        "draft_approve" | "merge_pr" | "draft_discard" => drafts::press(cmd, project, view),
        "draft_skip_design" | "board_design_write" => drafts::design(cmd, project, view),
        other => nack(&cmd.id, format!("Not a command this agent takes yet: {other}")),
    }
}

/// `release_status { part: "last" | "saved" | "note", id? }`: the Releases tab's reads of the shared backlog's saved notes,
/// queued for the project's loop, which has the store link and answers the `ack`'s `result`
/// ([`crate::read_kinds::release`]). Taken in `claim` and `runs` mode alike.
fn release_status(cmd: &Command, project: &str, view: &AgentView) -> Value {
    if let Err(reason) = acts_check(view) {
        return nack(&cmd.id, reason);
    }
    view.item_commands.push_read(crate::view::ReadCommand {
        project: project.to_string(),
        command_id: cmd.id.clone(),
        kind: cmd.name.clone(),
        args: cmd.args.clone(),
    });
    json!({ "type": PENDING, "id": cmd.id })
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

/// Pictures a message may carry (`shared/sentPictures.ts`, tray/05.3 section 5.3.4): at most four, by their store ids
/// (`pictureIds`); the loop fetches them (`orchestrator::driver::attached`).
pub(crate) const MAX_PICTURES: usize = 4;

/// **Continue** (`continue { itemId, notes?, pictureIds? }`) and **Start over** (`retry`, same arguments): queued for the
/// orchestrator loop (`orchestrator::driver::item_commands`), which carries it out on its next pass, after fetching
/// the pictures and adding their paths after the notes. Answers `pending` when there are pictures (the loop answers
/// once it has them, `ack`, or `nack` when the store won't give one), else `ack` at once. Refused outside `claim` mode,
/// the only mode that runs sessions.
fn item_command(cmd: &Command, project: &str, view: &AgentView, start_over: bool) -> Value {
    let id = cmd
        .args
        .get("itemId")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if id.is_empty() {
        let what = if start_over { "Start over" } else { "Continue" };
        return nack(&cmd.id, format!("{what} needs the item's id."));
    }
    if let Err(reason) = claim_check(view) {
        return nack(&cmd.id, reason);
    }
    let attached = match attach::attached(cmd) {
        Ok(a) => a,
        Err(reason) => return nack(&cmd.id, reason),
    };
    let pending = attached.is_some();
    queue_item(view, project, id, attach::notes_of(&cmd.args), attached, start_over);
    if pending {
        json!({ "type": PENDING, "id": cmd.id })
    } else {
        ack(&cmd.id)
    }
}

/// **playtest** (`playtest { itemId, mergedAt?, result: "pass" | "fail", notes?, pictureIds? }`), behind the `playtest-answer-command`
/// flag: a person's answer to a playtest, queued for the loop (which fetches the pictures, as [`item_command`]'s). Refused
/// outside `claim` and `runs` mode ([`acts_check`]: a `runs` computer holds no item, so the answer is a try-out's, which goes to the
/// store), and for an item the loop holds in a state other than `awaiting_playtest`.
fn playtest(cmd: &Command, project: &str, view: &AgentView) -> Value {
    let text = |k: &str| {
        cmd.args
            .get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };
    let id = text("itemId").unwrap_or("");
    if id.is_empty() {
        return nack(&cmd.id, "A playtest answer needs the item's id.".into());
    }
    let passed = match cmd.args.get("result").and_then(Value::as_str) {
        Some("pass") => true,
        Some("fail") => false,
        _ => return nack(&cmd.id, "A playtest answer's result must be pass or fail.".into()),
    };
    if let Err(reason) = acts_check(view) {
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
    let attached = match attach::attached(cmd) {
        Ok(a) => a,
        Err(reason) => return nack(&cmd.id, reason),
    };
    let pending = attached.is_some();
    view.item_commands.push(ItemCommand::Playtest {
        project: project.to_string(),
        id: id.to_string(),
        passed,
        notes: attach::notes_of(&cmd.args),
        attached,
        merged_at: text("mergedAt").map(str::to_string),
    });
    if pending {
        json!({ "type": PENDING, "id": cmd.id })
    } else {
        ack(&cmd.id)
    }
}

/// What [`run`] answers a command the loop settles once the store has answered: nothing is sent yet, and the loop sends
/// the `ack` or `nack` later ([`crate::view::ItemCommands::answer`]).
pub const PENDING: &str = "pending";

/// `setup_check {}` (**Check again**, first-run-check.md section 3.1): runs the fixed probes now. They take up to 20 s
/// each, so this answers `pending` and a task sends the `ack` with the checks as its `result`. The run goes through the
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
        commands.answer(&project, json!({ "type": "ack", "id": id, "result": checks.to_json() }));
    });
    json!({ "type": PENDING, "id": cmd.id })
}

/// Item commands only run where the orchestrator claims work. (Lane play, stop and restart also run on `runs`: [`acts_check`].)
fn claim_check(view: &AgentView) -> Result<(), String> {
    if view.item_commands.claim() {
        Ok(())
    } else {
        Err("This computer isn't running work (the orchestrator isn't in claim mode).".into())
    }
}

/// What isn't about an item (ideas, experts, drafts) also runs where the orchestrator runs on `runs`.
pub(crate) fn acts_check(view: &AgentView) -> Result<(), String> {
    if view.item_commands.acts() {
        Ok(())
    } else {
        Err("This computer isn't running work (the orchestrator isn't in claim or runs mode).".into())
    }
}

fn queue_item(
    view: &AgentView,
    project: &str,
    id: &str,
    notes: Option<String>,
    attached: Option<crate::view::Attached>,
    start_over: bool,
) {
    let (project, item) = (project.to_string(), id.to_string());
    view.item_commands.push(if start_over {
        ItemCommand::StartOver {
            project,
            item,
            notes,
            attached,
        }
    } else {
        ItemCommand::Continue {
            project,
            item,
            notes,
            attached,
        }
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
    queue_item(view, project, id, None, None, true);
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

/// **Skip** (`skip { itemId }`): queued for the orchestrator loop, which lets the item go and skips it in the store
/// (`orchestrator::driver::item_commands`). Refused outside `claim` mode, like Continue.
fn skip(cmd: &Command, project: &str, view: &AgentView) -> Value {
    let id = cmd.args.get("itemId").and_then(Value::as_str).unwrap_or("");
    answer(cmd, skip_item(view, project, id))
}

/// `play { lane? }` / `stop_play { lane? }` (also `stop`) / `restart_play { lane }`. Play and Stop with no lane are Play
/// main, lane 0 (`lane ?? 0` in the TS agent's `commands.ts`); Restart names its lane or its item. `itemId` (Play this
/// build, a build played in no lane) isn't taken here yet.
fn play(cmd: &Command, project: &str, view: &AgentView) -> Value {
    let lane = cmd.args.get("lane").and_then(Value::as_i64);
    let item = cmd
        .args
        .get("itemId")
        .and_then(Value::as_str)
        .is_some_and(|i| !i.is_empty());
    if item && lane.is_some() {
        return nack(&cmd.id, "Name an item or a lane, not both.".into());
    }
    if item {
        return nack(
            &cmd.id,
            format!("Not a command this agent takes yet: {} with an itemId", cmd.name),
        );
    }
    if cmd.name == "restart_play" && lane.is_none() {
        return nack(&cmd.id, "Name an item or a lane.".into());
    }
    let lane = lane.or(Some(0));
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
            acts_check(view)?;
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
            acts_check(view)?;
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
        _ => return Err("Name an item or a lane.".to_string()),
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

/// Whether `name` is a command only the agent's own cloud may send (the release note's page).
pub fn is_release_page(name: &str) -> bool {
    matches!(name, "release_publish" | "release_unpublish")
}

/// The refusal for such a command that came over another cloud's connection.
pub fn refuse_other_cloud(cmd: &Command) -> Value {
    nack(&cmd.id, release_publish::NOT_OWN_CLOUD.into())
}

/// `ack`, or `nack` with the reason.
fn answer(cmd: &Command, done: Result<(), String>) -> Value {
    match done {
        Ok(()) => ack(&cmd.id),
        Err(reason) => nack(&cmd.id, reason),
    }
}

pub(crate) fn ack(id: &str) -> Value {
    json!({ "type": "ack", "id": id })
}

pub(crate) fn nack(id: &str, reason: String) -> Value {
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
        "expert_start" => &["request"],
        "expert_helper" => &["task"],
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
/// ones and replays included, never an answer's content: a read command logs
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
    // `result` is the hub's name for what an `ack` carries (`shared/cloud.ts`); the expert loop still says `answer`.
    if let Some(result) = answer.get("result").or_else(|| answer.get("answer")) {
        entry.insert("answerBytes".into(), json!(result.to_string().len()));
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

mod attach;
mod computer;
mod drafts;
mod expert;
mod github;
mod item_actions;
mod link;
mod phone_play;
mod release_publish;
mod setup_tools;
#[cfg(test)]
mod tests;
mod unlink;
