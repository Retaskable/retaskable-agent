//! Commands from retaskable.com (cloud.md section 6, `agent/src/commands.ts`): checked in the
//! order section 6.2 gives (replay, expiry, Setup done, the command's own), then run and
//! answered with `ack` or `nack`. Ported one command at a time; `connector::receive` sends every
//! other command here too, so a name this agent doesn't run yet still gets the right nack and the
//! same replay/expiry checks as one it does.
//!
//! So far this runs **Restart**, **Pause**, **Resume** (`{ computer: true }` only: the
//! project-wide pause is the shared backlog's, answered by retaskable.com itself without
//! reaching a computer, agent/09-web-ui.md section 9.3) and **Shoot** (fire-and-forget: the ack
//! comes once `galleries::shoot` has started on its own thread, tray.md section 5.3.3). Continue
//! and Start over still need a real running implementer session (milestone 3 of
//! 11-build-milestones.md); Play still needs the build-and-install command play-on-phone.md
//! section 5 describes. Every command checked, run or refused is written to
//! `.retaskable/remote.log` ([`audit`]).

use std::collections::HashMap;
use std::path::Path;

use serde_json::{Map, Value, json};

use crate::connector::{AgentView, Exit};
use crate::galleries;
use crate::paths;
use crate::telemetry::events::{iso_utc_millis, now_ms};

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
        other => nack(&cmd.id, format!("Not a command this agent takes yet: {other}")),
    }
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

fn ack(id: &str) -> Value {
    json!({ "type": "ack", "id": id })
}

fn nack(id: &str, reason: String) -> Value {
    json!({ "type": "nack", "id": id, "reason": reason })
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
    entry.insert("args".into(), cmd.args.clone());
    entry.insert("outcome".into(), json!(outcome));
    if outcome == "nack" {
        entry.insert("reason".into(), answer.get("reason").cloned().unwrap_or(Value::Null));
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
    use crate::connector::ProjectView;

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
            }],
            version: "9.9.9".into(),
            lanes: 1,
            chore_lane: None,
            lane_sizes: Default::default(),
            paused: Default::default(),
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
        let v = view();
        let noop = |_: i32| {};
        let answer = run(
            &cmd("continue", json!({})),
            "one",
            &mut Seen::default(),
            &v,
            &noop,
            &data_dir(),
        );
        assert_eq!(answer["reason"], "Not a command this agent takes yet: continue");
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
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !index.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(index.exists());
        assert_eq!(
            galleries::status("commands-shoot-happy-path", "models"),
            galleries::GalleryStatus::Idle
        );
    }
}
