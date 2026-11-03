//! Commands from retaskable.com (cloud.md section 6, `agent/src/commands.ts`): checked in the
//! order section 6.2 gives (replay, expiry, Setup done, the command's own), then run and
//! answered with `ack` or `nack`. Ported one command at a time; `connector::receive` sends every
//! other command here too, so a name this agent doesn't run yet still gets the right nack and the
//! same replay/expiry checks as one it does.
//!
//! So far this runs **Continue** and **Start over** (`retry`) (queued for the orchestrator loop, in `claim` mode only: the loop moves the item,
//! `orchestrator::driver::item_commands`), **Restart**, **Pause**, **Resume** (`{ computer: true }` only: the
//! project-wide pause is the shared backlog's, answered by retaskable.com itself without
//! reaching a computer, agent/09-web-ui.md section 9.3) and **Shoot** (fire-and-forget: the ack
//! comes once `galleries::shoot` has started on its own thread, tray.md section 5.3.3) and
//! **Play main** / **Stop** (`{ lane: 0 }`, [`crate::play`]) and **draft_files** (answered live from the draft's worktree), **draft_edit** and **draft_discard** ([`crate::drafts::actions`]) and **draft_approve** / **merge_pr** `{ draftId }` (**Approve** and **Merge into main** on a spec draft, queued for the loop, `orchestrator::driver::land`). Play on a lane
//! is a later piece. Every command checked, run or refused is written to
//! `.retaskable/remote.log` ([`audit`]).

use std::collections::HashMap;
use std::path::Path;

use serde_json::{Map, Value, json};

use crate::galleries;
use crate::paths;
use crate::telemetry::events::{iso_utc_millis, now_ms};
use crate::view::{AgentView, DraftCommand, Exit, ItemCommand};

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
            view.paused.set(
                project,
                Some(format!("Paused from retaskable.com by {}", cmd.issued_by)),
            );
            ack(&cmd.id)
        }
        "resume" if computer => {
            view.paused.set(project, None);
            ack(&cmd.id)
        }
        "pause" | "resume" => nack(&cmd.id, SITE_ANSWERS.to_string()),
        "shoot" => shoot(cmd, project, view, data_dir),
        "continue" => item_command(cmd, project, view, data_dir, false),
        "retry" => item_command(cmd, project, view, data_dir, true),
        "play" | "stop" => play(cmd, project, view),
        "draft_files" => draft_files(cmd, project, view),
        "draft_edit" | "draft_discard" => draft_change(cmd, project, view),
        "draft_approve" | "merge_pr" => draft_press(cmd, project, view),
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
    if !view.item_commands.claim() {
        return nack(
            &cmd.id,
            "This computer isn't running work (the orchestrator isn't in claim mode).".into(),
        );
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
    let (project, item, notes) = (
        project.to_string(),
        id.to_string(),
        (!notes.is_empty()).then_some(notes),
    );
    view.item_commands.push(if start_over {
        ItemCommand::StartOver { project, item, notes }
    } else {
        ItemCommand::Continue { project, item, notes }
    });
    ack(&cmd.id)
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

/// `play { lane: 0 }` / `stop { lane: 0 }`: Play main ([`crate::play`]). Any other lane is a later piece.
fn play(cmd: &Command, project: &str, view: &AgentView) -> Value {
    let lane = cmd.args.get("lane").and_then(Value::as_i64);
    if lane != Some(0) {
        return nack(
            &cmd.id,
            format!("Not a command this agent takes yet: {} on a lane", cmd.name),
        );
    }
    let Some(p) = view.projects.iter().find(|p| p.id == project) else {
        return nack(&cmd.id, "Finish setup first.".to_string());
    };
    let done = if cmd.name == "play" {
        crate::play::start(project, p)
    } else {
        crate::play::stop(project)
    };
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
/// the draft's PR branch, and answers with the files listed again (`answer.files`). `draft_discard { id }` closes
/// the PR with `gh`, deletes its branch and removes the worktree, then asks the project's loop to fetch again.
/// `branch` may name the draft's branch; without it the checkout's `retaskable/spec-<id>-…` is used.
fn draft_change(cmd: &Command, project: &str, view: &AgentView) -> Value {
    use crate::drafts::actions::{Where, branch_of, discard, edit};
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
    if cmd.name == "draft_discard" {
        let gh = std::env::var_os("RETASKABLE_GH").map_or_else(|| "gh".into(), std::path::PathBuf::from);
        return match discard(&w, &gh, id, &branch) {
            Ok(()) => {
                view.work.wake(project);
                ack(&cmd.id)
            }
            Err(e) => nack(&cmd.id, format!("Couldn't discard draft {id}: {e:#}")),
        };
    }
    let (Some(file), Some(content)) = (arg("path"), arg("content")) else {
        return nack(&cmd.id, "draft_edit needs the file's path and content.".into());
    };
    match edit(&w, id, &branch, file, content) {
        Ok(files) => json!({ "type": "ack", "id": cmd.id, "answer": { "files": files } }),
        Err(e) => nack(&cmd.id, format!("Couldn't edit draft {id}: {e:#}")),
    }
}

/// `draft_approve { draftId }` (**Approve and merge**) or `merge_pr { draftId }` (**Merge into main**): queued for the
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
        return nack(&cmd.id, "Approve needs the draft's id.".into());
    }
    if !view.item_commands.claim() {
        return nack(
            &cmd.id,
            "This computer isn't running work (the orchestrator isn't in claim mode).".into(),
        );
    }
    let (project, draft) = (project.to_string(), draft.to_string());
    view.item_commands.push_draft(if cmd.name == "merge_pr" {
        DraftCommand::Merge { project, draft }
    } else {
        DraftCommand::Approve { project, draft }
    });
    ack(&cmd.id)
}

fn ack(id: &str) -> Value {
    json!({ "type": "ack", "id": id })
}

fn nack(id: &str, reason: String) -> Value {
    json!({ "type": "nack", "id": id, "reason": reason })
}

/// A command's args for the log: pictures as `{ count, bytes }` only, never their data (tray/05.3 section 5.3.4).
fn audit_args(args: &Value) -> Value {
    let mut args = args.clone();
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
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::view::ProjectView;

    fn view() -> AgentView {
        AgentView {
            machine: "pc".into(),
            tags: vec![],
            projects: vec![ProjectView {
                id: "one".into(),
                github_repo: "o/one".into(),
                backlog_path: "backlog".into(),
                repo_path: PathBuf::new(),
                worktrees_dir: PathBuf::new(),
                base_branch: String::new(),
                galleries: vec![],
                play_command: String::new(),
            }],
            version: "9.9.9".into(),
            lanes: 1,
            chore_lane: None,
            lane_sizes: Default::default(),
            paused: Default::default(),
            item_commands: Default::default(),
            work: Default::default(),
            restart: Default::default(),
        }
    }

    fn data_dir() -> PathBuf {
        std::env::temp_dir().join("retaskable-commands-test")
    }

    fn cmd(name: &str, args: Value) -> Command {
        Command {
            id: "c1".into(),
            name: name.into(),
            args,
            issued_by: "jc".into(),
            issued_at: now_ms(),
            expires_at: now_ms() + 60_000,
        }
    }

    #[test]
    fn restart_acks_and_calls_exit_with_75() {
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let kept = calls.clone();
        let exit = move |code: i32| kept.lock().unwrap().push(code);
        let answer = run(
            &cmd("restart", json!({})),
            "one",
            &mut Seen::default(),
            &view(),
            &exit,
            &data_dir(),
        );
        assert_eq!(answer, json!({ "type": "ack", "id": "c1" }));
        assert_eq!(*calls.lock().unwrap(), vec![75]);
    }

    #[test]
    fn pause_and_resume_need_computer_true_else_the_site_answered_it() {
        let v = view();
        let noop = |_: i32| {};
        let answer = run(
            &cmd("pause", json!({})),
            "one",
            &mut Seen::default(),
            &v,
            &noop,
            &data_dir(),
        );
        assert_eq!(answer["reason"], SITE_ANSWERS);
        assert_eq!(v.paused.reason("one"), None);

        let answer = run(
            &cmd("pause", json!({ "computer": true })),
            "one",
            &mut Seen::default(),
            &v,
            &noop,
            &data_dir(),
        );
        assert_eq!(answer, json!({ "type": "ack", "id": "c1" }));
        assert_eq!(
            v.paused.reason("one"),
            Some("Paused from retaskable.com by jc".to_string())
        );

        let answer = run(
            &cmd("resume", json!({ "computer": true })),
            "one",
            &mut Seen::default(),
            &v,
            &noop,
            &data_dir(),
        );
        assert_eq!(answer, json!({ "type": "ack", "id": "c1" }));
        assert_eq!(v.paused.reason("one"), None);
    }

    #[test]
    fn a_replayed_id_is_refused_the_second_time() {
        let v = view();
        let noop = |_: i32| {};
        let mut seen = Seen::default();
        assert_eq!(
            run(&cmd("restart", json!({})), "one", &mut seen, &v, &noop, &data_dir())["type"],
            "ack"
        );
        let answer = run(&cmd("restart", json!({})), "one", &mut seen, &v, &noop, &data_dir());
        assert_eq!(answer["reason"], "Already received: a command runs once.");
    }

    #[test]
    fn an_expired_command_is_refused() {
        let v = view();
        let noop = |_: i32| {};
        let mut old = cmd("restart", json!({}));
        old.issued_at = now_ms() - 61_000;
        old.expires_at = old.issued_at + 60_000;
        let answer = run(&old, "one", &mut Seen::default(), &v, &noop, &data_dir());
        assert!(answer["reason"].as_str().unwrap().starts_with("Expired"));
    }

    #[test]
    fn before_setup_only_the_exempt_commands_run() {
        let v = view();
        let noop = |_: i32| {};
        let answer = run(
            &cmd("restart", json!({})),
            "",
            &mut Seen::default(),
            &v,
            &noop,
            &data_dir(),
        );
        assert_eq!(answer["reason"], "Finish setup first.");
        let answer = run(
            &cmd("setup_check", json!({})),
            "",
            &mut Seen::default(),
            &v,
            &noop,
            &data_dir(),
        );
        assert_eq!(answer["reason"], "Not a command this agent takes yet: setup_check");
    }

    #[test]
    fn an_unported_command_is_named_in_its_nack() {
        let cmd = |name| cmd(name, json!({}));
        let v = view();
        let noop = |_: i32| {};
        let answer = run(&cmd("start_over"), "one", &mut Seen::default(), &v, &noop, &data_dir());
        assert_eq!(answer["reason"], "Not a command this agent takes yet: start_over");
    }

    #[test]
    fn play_and_stop_nack_a_lane_other_than_zero_and_a_blank_command() {
        let v = view();
        let noop = |_: i32| {};
        for name in ["play", "stop"] {
            let answer = run(
                &cmd(name, json!({ "lane": 2 })),
                "one",
                &mut Seen::default(),
                &v,
                &noop,
                &data_dir(),
            );
            assert_eq!(
                answer["reason"],
                format!("Not a command this agent takes yet: {name} on a lane")
            );
        }
        let answer = run(
            &cmd("play", json!({ "lane": 0 })),
            "one",
            &mut Seen::default(),
            &v,
            &noop,
            &data_dir(),
        );
        assert_eq!(answer["reason"], "No play command set for this project.");
    }

    #[test]
    fn shoot_nacks_an_unknown_gallery() {
        let v = view();
        let noop = |_: i32| {};
        let answer = run(
            &cmd("shoot", json!({ "gallery": "intro" })),
            "one",
            &mut Seen::default(),
            &v,
            &noop,
            &data_dir(),
        );
        assert_eq!(answer["reason"], "No gallery called intro.");
    }

    #[test]
    fn shoot_nacks_a_gallery_with_no_command_set() {
        let mut v = view();
        v.projects[0].galleries = vec![crate::config::GalleryConfig {
            name: "Intro".into(),
            command: String::new(),
            out_dir: "shots".into(),
        }];
        let noop = |_: i32| {};
        let answer = run(
            &cmd("shoot", json!({ "gallery": "intro" })),
            "one",
            &mut Seen::default(),
            &v,
            &noop,
            &data_dir(),
        );
        assert_eq!(answer["reason"], "No command set for this gallery.");
    }

    #[cfg(windows)]
    fn copy_fixture_command() -> String {
        "mkdir shots && copy fixture.png shots\\01-first.png".to_string()
    }
    #[cfg(not(windows))]
    fn copy_fixture_command() -> String {
        "mkdir -p shots && cp fixture.png shots/01-first.png".to_string()
    }

    #[test]
    fn shoot_acks_at_once_and_runs_the_gallery_in_the_background() {
        let root = tempfile::tempdir().unwrap();
        let repo = crate::test_support::repo_with_origin(root.path());
        image::DynamicImage::ImageRgb8(image::RgbImage::new(100, 20))
            .save(repo.join("fixture.png"))
            .unwrap();
        crate::test_support::run(crate::test_support::git(&repo).args(["add", "fixture.png"]));
        crate::test_support::run(crate::test_support::git(&repo).args(["commit", "--quiet", "-m", "fixture"]));
        crate::test_support::run(crate::test_support::git(&repo).args(["push", "--quiet", "origin", "main"]));

        let mut v = view();
        v.projects[0].id = "commands-shoot-happy-path".into();
        v.projects[0].repo_path = repo.clone();
        v.projects[0].worktrees_dir = root.path().join("worktrees");
        v.projects[0].base_branch = "main".into();
        v.projects[0].galleries = vec![crate::config::GalleryConfig {
            name: "Models".into(),
            command: copy_fixture_command(),
            out_dir: "shots".into(),
        }];
        let noop = |_: i32| {};
        let dir = root.path().join("data");

        let answer = run(
            &cmd("shoot", json!({ "gallery": "models" })),
            "commands-shoot-happy-path",
            &mut Seen::default(),
            &v,
            &noop,
            &dir,
        );
        assert_eq!(answer, json!({ "type": "ack", "id": "c1" }));

        let index = dir.join("projects/commands-shoot-happy-path/galleries/models/index.json");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
        while !index.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(index.exists());
        assert_eq!(
            galleries::status("commands-shoot-happy-path", "models"),
            galleries::GalleryStatus::Idle
        );
    }

    fn claiming() -> AgentView {
        let v = view();
        v.item_commands.set_claim(true);
        v
    }

    fn go(v: &AgentView, args: Value, dir: &Path) -> Value {
        run(&cmd("continue", args), "one", &mut Seen::default(), v, &|_| {}, dir)
    }

    fn picture(bytes: usize) -> Value {
        use base64::Engine;
        let data = base64::engine::general_purpose::STANDARD.encode(vec![7u8; bytes]);
        json!({ "type": "image/png", "data": data })
    }

    #[test]
    fn continue_queues_the_notes_and_acks() {
        let v = claiming();
        let answer = go(&v, json!({ "id": "a", "notes": " Try again. " }), &data_dir());
        assert_eq!(answer, json!({ "type": "ack", "id": "c1" }));
        assert_eq!(
            v.item_commands.take("one"),
            vec![ItemCommand::Continue {
                project: "one".into(),
                item: "a".into(),
                notes: Some("Try again.".into())
            }]
        );
    }

    #[test]
    fn continue_without_notes_queues_none() {
        let v = claiming();
        go(&v, json!({ "id": "a" }), &data_dir());
        let ItemCommand::Continue { notes, .. } = v.item_commands.take("one").remove(0) else {
            panic!()
        };
        assert_eq!(notes, None);
    }

    #[test]
    fn retry_queues_start_over_with_the_same_checks_and_pictures() {
        let v = claiming();
        let run = |args| {
            run(
                &cmd("retry", args),
                "one",
                &mut Seen::default(),
                &v,
                &|_| {},
                &data_dir(),
            )
        };
        assert_eq!(run(json!({ "id": "a", "notes": " Go again. " }))["type"], "ack");
        assert_eq!(
            v.item_commands.take("one"),
            vec![ItemCommand::StartOver {
                project: "one".into(),
                item: "a".into(),
                notes: Some("Go again.".into())
            }]
        );
        assert!(
            run(json!({ "id": " " }))["reason"]
                .as_str()
                .unwrap()
                .contains("Start over")
        );
        assert_eq!(run(json!({ "id": "a", "pictures": [picture(10)] }))["type"], "ack");
        let ItemCommand::StartOver { notes, .. } = v.item_commands.take("one").remove(0) else {
            panic!()
        };
        assert!(notes.unwrap().starts_with("Picture: "));
        let off = view();
        let answer = super::run(
            &cmd("retry", json!({ "id": "a" })),
            "one",
            &mut Seen::default(),
            &off,
            &|_| {},
            &data_dir(),
        );
        assert_eq!(answer["type"], "nack");
    }

    #[test]
    fn continue_needs_an_id_and_claim_mode() {
        let v = claiming();
        assert!(
            go(&v, json!({}), &data_dir())["reason"]
                .as_str()
                .unwrap()
                .contains("id")
        );
        assert!(go(&v, json!({ "id": "  " }), &data_dir())["type"] == "nack");
        let off = view();
        let answer = go(&off, json!({ "id": "a" }), &data_dir());
        assert!(answer["reason"].as_str().unwrap().contains("claim mode"));
        assert!(off.item_commands.take("one").is_empty());
    }

    #[test]
    fn continue_saves_pictures_under_attachments_and_adds_their_paths_after_the_notes() {
        let v = claiming();
        let dir = tempfile::tempdir().unwrap();
        let answer = go(
            &v,
            json!({ "id": "a", "notes": "See this.", "pictures": [picture(10), picture(20)] }),
            dir.path(),
        );
        assert_eq!(answer["type"], "ack");
        let ItemCommand::Continue { notes, .. } = v.item_commands.take("one").remove(0) else {
            panic!()
        };
        let notes = notes.unwrap();
        let files: Vec<_> = std::fs::read_dir(dir.path().join("attachments")).unwrap().collect();
        assert_eq!(files.len(), 2);
        for f in files {
            let path = f.unwrap().path();
            assert!(notes.contains(&path.display().to_string()));
            assert_eq!(path.extension().unwrap(), "png");
        }
        assert!(notes.starts_with("See this.\nPicture: "));
    }

    #[test]
    fn a_bad_or_oversized_picture_nacks_and_saves_nothing() {
        let v = claiming();
        let dir = tempfile::tempdir().unwrap();
        let too_many = json!([picture(1), picture(1), picture(1), picture(1), picture(1)]);
        let bad_type = json!([picture(1), { "type": "image/bmp", "data": "AAAA" }]);
        let bad_data = json!([picture(1), { "type": "image/png", "data": "***" }]);
        let big = json!([picture(1), picture((1 << 20) + 1)]);
        let in_all = json!([
            picture(1 << 20),
            picture(1 << 20),
            picture(1 << 20),
            picture(1 << 20),
            picture(1)
        ]);
        for pictures in [too_many, bad_type, bad_data, big, in_all] {
            let answer = go(&v, json!({ "id": "a", "pictures": pictures }), dir.path());
            assert_eq!(answer["type"], "nack", "{answer}");
        }
        assert!(!dir.path().join("attachments").exists());
        assert!(v.item_commands.take("one").is_empty());
    }

    #[test]
    fn the_audit_log_holds_a_pictures_size_not_its_bytes() {
        let args = json!({ "id": "a", "pictures": [{ "type": "image/png", "data": "AAAA" }] });
        assert_eq!(audit_args(&args)["pictures"], json!({ "count": 1, "bytes": 4 }));
    }

    #[test]
    fn draft_files_answers_from_the_drafts_worktree_and_is_audited() {
        use crate::test_support::{git, repo_with_origin, run};
        let tmp = tempfile::tempdir().unwrap();
        let repo = repo_with_origin(tmp.path());
        let worktrees = tmp.path().join("worktrees");
        let wt = crate::draft_worktree::path(&worktrees, "7");
        run(git(&repo).args(["worktree", "add", "--quiet", "-b", "spec-7"]).arg(&wt));
        std::fs::write(wt.join("new.md"), "# New\n").unwrap();
        run(git(&wt).args(["add", "new.md"]));
        run(git(&wt).args(["commit", "--quiet", "-m", "draft"]));
        let mut v = view();
        v.projects[0].worktrees_dir = worktrees;
        v.projects[0].base_branch = "main".into();
        let noop = |_: i32| {};
        let c = cmd("draft_files", json!({ "id": "7" }));
        let answer = run_cmd(&c, &v, &noop);
        assert_eq!(answer["type"], "ack", "{answer}");
        assert_eq!(
            answer["answer"]["files"],
            json!([{ "path": "new.md", "content": "# New\n" }])
        );

        audit(tmp.path(), &c, &answer, "wss://x");
        let log = std::fs::read_to_string(tmp.path().join("remote.log")).unwrap();
        assert!(log.contains("\"name\":\"draft_files\"") && log.contains("\"outcome\":\"ack\""));
        assert!(!log.contains("# New"));
    }

    #[cfg(unix)]
    #[test]
    fn draft_edit_commits_and_draft_discard_closes() {
        use crate::test_support::{git, repo_with_origin, run, write_executable};
        let tmp = tempfile::tempdir().unwrap();
        let repo = repo_with_origin(tmp.path());
        let worktrees = tmp.path().join("worktrees");
        let wt = crate::draft_worktree::path(&worktrees, "7");
        run(git(&repo)
            .args(["worktree", "add", "--quiet", "-b", "retaskable/spec-7-x"])
            .arg(&wt));
        let gh = tmp.path().join("gh");
        write_executable(&gh, "#!/bin/sh\nexit 0\n");
        let mut v = view();
        v.projects[0].repo_path = repo;
        v.projects[0].worktrees_dir = worktrees;
        v.projects[0].base_branch = "main".into();
        let noop = |_: i32| {};
        let edit = run_cmd(
            &cmd("draft_edit", json!({ "id": "7", "path": "n.md", "content": "hi\n" })),
            &v,
            &noop,
        );
        assert_eq!(edit["type"], "ack", "{edit}");
        assert_eq!(edit["answer"]["files"], json!([{ "path": "n.md", "content": "hi\n" }]));
        let missing = run_cmd(&cmd("draft_edit", json!({ "id": "7" })), &v, &noop);
        assert_eq!(missing["type"], "nack");
        let unknown = run_cmd(&cmd("draft_discard", json!({ "id": "8" })), &v, &noop);
        assert_eq!(unknown["type"], "nack");
        // SAFETY: tests that set RETASKABLE_GH are the only ones that read it.
        unsafe { std::env::set_var("RETASKABLE_GH", &gh) };
        let gone = run_cmd(&cmd("draft_discard", json!({ "id": "7" })), &v, &noop);
        assert_eq!(gone["type"], "ack", "{gone}");
        assert!(!wt.exists());
    }

    #[test]
    fn draft_files_nacks_an_unknown_draft() {
        let tmp = tempfile::tempdir().unwrap();
        let mut v = view();
        v.projects[0].worktrees_dir = tmp.path().to_path_buf();
        let noop = |_: i32| {};
        for args in [json!({ "id": "9" }), json!({}), json!({ "id": "../x" })] {
            let answer = run_cmd(&cmd("draft_files", args), &v, &noop);
            assert_eq!(answer["type"], "nack", "{answer}");
        }
    }

    #[test]
    fn approve_and_merge_into_main_are_queued_for_the_loop() {
        let v = view();
        let noop = |_: i32| {};
        let args = json!({ "draftId": "d1" });
        assert_eq!(run_cmd(&cmd("draft_approve", args.clone()), &v, &noop)["type"], "nack");
        v.item_commands.set_claim(true);
        assert_eq!(run_cmd(&cmd("draft_approve", args.clone()), &v, &noop)["type"], "ack");
        assert_eq!(run_cmd(&cmd("merge_pr", args), &v, &noop)["type"], "ack");
        assert_eq!(run_cmd(&cmd("draft_approve", json!({})), &v, &noop)["type"], "nack");
        assert_eq!(
            run_cmd(&cmd("merge_pr", json!({ "itemId": "x" })), &v, &noop)["type"],
            "nack"
        );
        assert_eq!(
            v.item_commands.take_drafts("one"),
            [
                DraftCommand::Approve {
                    project: "one".into(),
                    draft: "d1".into()
                },
                DraftCommand::Merge {
                    project: "one".into(),
                    draft: "d1".into()
                }
            ]
        );
    }

    fn run_cmd(c: &Command, v: &AgentView, exit: &Exit) -> Value {
        run(c, "one", &mut Seen::default(), v, exit, &data_dir())
    }
}
