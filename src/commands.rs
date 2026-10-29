//! Commands from retaskable.com (cloud.md section 6, `agent/src/commands.ts`): checked in the
//! order section 6.2 gives (replay, expiry, Setup done, the command's own), then run and
//! answered with `ack` or `nack`. Ported one command at a time; `connector::receive` sends every
//! other command here too, so a name this agent doesn't run yet still gets the right nack and the
//! same replay/expiry checks as one it does.
//!
//! So far this runs **Restart**, **Pause** and **Resume** (`{ computer: true }` only: the
//! project-wide pause is the shared backlog's, answered by retaskable.com itself without
//! reaching a computer, agent/09-web-ui.md section 9.3). Continue, Start over, Shoot and Play
//! still need the orchestrator loop and lanes (milestones 4 and 5) this agent doesn't have yet.

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::connector::{AgentView, Exit};

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

/// Now, in epoch milliseconds, for the expiry check.
pub fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
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
/// `seen` and `view.paused` persist for the project's connection, across reconnects.
pub fn run(cmd: &Command, project: &str, seen: &mut Seen, view: &AgentView, exit: &Exit) -> Value {
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
        other => nack(&cmd.id, format!("Not a command this agent takes yet: {other}")),
    }
}

fn ack(id: &str) -> Value {
    json!({ "type": "ack", "id": id })
}

fn nack(id: &str, reason: String) -> Value {
    json!({ "type": "nack", "id": id, "reason": reason })
}

#[cfg(test)]
mod tests {
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
            }],
            version: "9.9.9".into(),
            lanes: 1,
            chore_lane: None,
            lane_sizes: Default::default(),
            paused: Default::default(),
        }
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
        let answer = run(&cmd("restart", json!({})), "one", &mut Seen::default(), &view(), &exit);
        assert_eq!(answer, json!({ "type": "ack", "id": "c1" }));
        assert_eq!(*calls.lock().unwrap(), vec![75]);
    }

    #[test]
    fn pause_and_resume_need_computer_true_else_the_site_answered_it() {
        let v = view();
        let noop = |_: i32| {};
        let answer = run(&cmd("pause", json!({})), "one", &mut Seen::default(), &v, &noop);
        assert_eq!(answer["reason"], SITE_ANSWERS);
        assert_eq!(v.paused.reason("one"), None);

        let answer = run(
            &cmd("pause", json!({ "computer": true })),
            "one",
            &mut Seen::default(),
            &v,
            &noop,
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
            run(&cmd("restart", json!({})), "one", &mut seen, &v, &noop)["type"],
            "ack"
        );
        let answer = run(&cmd("restart", json!({})), "one", &mut seen, &v, &noop);
        assert_eq!(answer["reason"], "Already received: a command runs once.");
    }

    #[test]
    fn an_expired_command_is_refused() {
        let v = view();
        let noop = |_: i32| {};
        let mut old = cmd("restart", json!({}));
        old.issued_at = now_ms() - 61_000;
        old.expires_at = old.issued_at + 60_000;
        let answer = run(&old, "one", &mut Seen::default(), &v, &noop);
        assert!(answer["reason"].as_str().unwrap().starts_with("Expired"));
    }

    #[test]
    fn before_setup_only_the_exempt_commands_run() {
        let v = view();
        let noop = |_: i32| {};
        let answer = run(&cmd("restart", json!({})), "", &mut Seen::default(), &v, &noop);
        assert_eq!(answer["reason"], "Finish setup first.");
        let answer = run(&cmd("setup_check", json!({})), "", &mut Seen::default(), &v, &noop);
        assert_eq!(answer["reason"], "Not a command this agent takes yet: setup_check");
    }

    #[test]
    fn an_unported_command_is_named_in_its_nack() {
        let v = view();
        let noop = |_: i32| {};
        let answer = run(
            &cmd("shoot", json!({ "gallery": "intro" })),
            "one",
            &mut Seen::default(),
            &v,
            &noop,
        );
        assert_eq!(answer["reason"], "Not a command this agent takes yet: shoot");
    }
}
