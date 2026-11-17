//! Whether this computer may release a project's work (`agent/src/releaseGate.ts`, release-allowed-switch.md sections 2
//! and 3.1): the switch, its owner's role on the project (told by the hub, `owner_role`) and this computer's `gh` login
//! passing environments.md section 4.2's admin check. The login's answer is cached: asked again after ten minutes, kept
//! for thirty when GitHub can't be reached, then false. Nothing here starts work: it only says what `hello` reports.
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{Notify, watch};

use crate::config::{self, ProjectConfig};
use crate::sync::lock;

/// How old the login's answer may be before it is asked again.
pub const ADMIN_CHECK: Duration = Duration::from_secs(10 * 60);
/// How long the last good answer stands when GitHub can't be reached, then it reads false.
pub const ADMIN_KEEP: Duration = Duration::from_secs(30 * 60);

/// The owner's role on a project (`ProjectRole`): what the hub says in `owner_role`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Member,
    Admin,
}

impl Role {
    /// `owner_role`'s `role`: `Some(None)` for the hub's `null` (no role), `None` for anything it doesn't say.
    pub fn read(v: &serde_json::Value) -> Option<Option<Role>> {
        match v {
            serde_json::Value::Null => Some(None),
            serde_json::Value::String(s) if s == "admin" => Some(Some(Role::Admin)),
            serde_json::Value::String(s) if s == "member" => Some(Some(Role::Member)),
            _ => None,
        }
    }
}

/// What the answer needs from the project and the machine, read by the caller.
pub struct Inputs<'a> {
    /// The machine's switch (`config::releases_switch`): `Some(false)` is off for every project.
    pub switch: Option<bool>,
    pub repo: &'a str,
    /// The project has a rollout (`rollout_tools::rollout_of`), the rule before the hub says the owner's role.
    pub rollout: bool,
    /// What stops this computer running its rollout commands (`rollout_tools::missing`).
    pub missing_tools: Option<String>,
}

/// Whether the computer may release the project, and when it may not, why (the settings page's state line).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub on: bool,
    pub why: Option<String>,
}

impl Resolved {
    fn on() -> Self {
        Self { on: true, why: None }
    }
    fn off(why: Option<String>) -> Self {
        Self { on: false, why }
    }
}

#[derive(Default)]
struct State {
    roles: HashMap<String, Option<Role>>,
    logins: HashMap<String, (bool, Instant)>,
}

impl State {
    /// The login's answer as `admin` gives it, without asking again.
    fn known(&self, project: &str, now: Instant) -> Option<bool> {
        let (admin, at) = *self.logins.get(project)?;
        Some(now.saturating_duration_since(at) < ADMIN_KEEP && admin)
    }
}

struct Inner {
    state: Mutex<State>,
    /// Counts each change to an answer, so every connection can send its hello again.
    changed: watch::Sender<u64>,
    /// Wakes [`run`] to ask now: a project with no answer yet, or one gone stale.
    kick: Notify,
}

/// Cheap to clone; clones share the answers.
#[derive(Clone)]
pub struct ReleaseGate(Arc<Inner>);

impl Default for ReleaseGate {
    fn default() -> Self {
        Self(Arc::new(Inner {
            state: Mutex::new(State::default()),
            changed: watch::Sender::new(0),
            kick: Notify::new(),
        }))
    }
}

impl std::fmt::Debug for ReleaseGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ReleaseGate(..)")
    }
}

impl ReleaseGate {
    /// Wakes on every change to an answer (the hello goes again if it differs).
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.0.changed.subscribe()
    }

    fn fire(&self) {
        self.0.changed.send_modify(|n| *n += 1);
    }

    /// The hub's word on the owner's role; wakes the hello when it changed.
    pub fn set_role(&self, project: &str, role: Option<Role>) -> bool {
        let was = lock(&self.0.state).roles.insert(project.to_string(), role);
        let changed = was != Some(role);
        if changed {
            self.fire();
        }
        changed
    }

    /// The hub's word on the role, if it has said.
    pub fn role(&self, project: &str) -> Option<Option<Role>> {
        lock(&self.0.state).roles.get(project).copied()
    }

    /// Notes the login's answer for `project`, waking the hello when what `admin` reads moved.
    pub fn record(&self, project: &str, admin: bool, now: Instant) {
        let moved = {
            let mut s = lock(&self.0.state);
            let before = s.known(project, now);
            s.logins.insert(project.to_string(), (admin, now));
            before != s.known(project, now)
        };
        if moved {
            self.fire();
        }
    }

    /// After a check that could not reach GitHub or finished: wakes the hello if an old answer has since lapsed.
    pub fn lapse(&self, project: &str, was: Option<bool>, now: Instant) {
        if was != lock(&self.0.state).known(project, now) {
            self.fire();
        }
    }

    /// The login's answer for `project` now, or `None` before the first one. Asks for it again in the background
    /// once it's ten minutes old.
    pub fn admin(&self, project: &str, now: Instant) -> Option<bool> {
        let (seen, stale) = {
            let s = lock(&self.0.state);
            let at = s.logins.get(project).map(|(_, at)| *at);
            (
                s.known(project, now),
                at.is_none_or(|at| now.saturating_duration_since(at) >= ADMIN_CHECK),
            )
        };
        if stale {
            self.0.kick.notify_one();
        }
        seen
    }

    /// `ReleaseGate.resolve`: may the computer release `project`, and if not, why.
    pub fn resolve(&self, project: &str, input: &Inputs<'_>, now: Instant) -> Resolved {
        let Some(role) = self.role(project) else {
            // The hub didn't say (an older hub): the switch, then the project's rollout.
            return if input.switch != Some(false) && input.rollout {
                Resolved::on()
            } else {
                Resolved::off(None)
            };
        };
        let repo = input.repo;
        if input.switch == Some(false) {
            return Resolved::off(None);
        }
        if role != Some(Role::Admin) {
            return Resolved::off(Some(format!("Off: you're not an admin of {repo}")));
        }
        match self.admin(project, now) {
            None => Resolved::off(Some(format!("Off: checking this computer's GitHub login for {repo}"))),
            Some(false) => Resolved::off(Some(format!(
                "Off: this computer's GitHub login isn't an admin of {repo}"
            ))),
            Some(true) => match &input.missing_tools {
                Some(missing) => Resolved::off(Some(format!("Off: {missing}"))),
                None => Resolved::on(),
            },
        }
    }
}

/// Checks every linked project's login now, then every ten minutes (or sooner when asked), so an idle computer notices a
/// login that lost admin, or GitHub being out for thirty minutes, without anything asking. Runs until dropped.
pub async fn run(gate: ReleaseGate, dir: PathBuf) {
    loop {
        let projects: Vec<ProjectConfig> = config::load(&dir).map(|c| c.projects).unwrap_or_default();
        for p in projects.iter().filter(|p| !p.github_repo.is_empty()) {
            let id = p.id_or_derived();
            let was = lock(&gate.0.state).known(&id, Instant::now());
            match crate::promote::host::permissions(
                std::path::Path::new("gh"),
                std::path::Path::new(&p.repo_path),
                &p.github_repo,
            )
            .await
            {
                Ok((admin, _)) => gate.record(&id, admin, Instant::now()),
                // GitHub can't be reached: the last answer stands until it's thirty minutes old.
                Err(_) => gate.lapse(&id, was, Instant::now()),
            }
        }
        tokio::select! {
            () = tokio::time::sleep(ADMIN_CHECK) => {},
            () = gate.0.kick.notified() => {},
        }
    }
}

#[cfg(test)]
mod tests;
