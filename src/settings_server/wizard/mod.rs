//! The setup wizard on the settings page (docs/specs/setup-wizard.md section 3): `GET /api/wizard` reads what this
//! computer has and says, step by step, what is done, what needs the person and what failed and why; the saves behind
//! its buttons (`POST /api/wizard/github`, `/claude`, `/claude/test`, `/ci`) are settings changes of the kind the page
//! already makes, behind the same session, CSRF, Host, Origin and this-computer checks. It adds no command and no port:
//! linking and Add project are the page's own routes. No value of a token is ever in an answer or a log line.
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

use super::SettingsServer;

mod gather;
pub mod probes;
mod routes;
pub mod steps;
#[cfg(test)]
mod tests;

pub(super) use routes::{get_wizard, post_ci, post_claude, post_claude_test, post_github};

use probes::{Probes, RealProbes};
use steps::{ClaudeStatus, GhOk};

/// Whether the last visit found every step done: the one thing the first-visit door reads.
const NOTE_FILE: &str = "wizard.json";

/// How long a GitHub or Claude answer is kept before the page asks again (unless the person pressed check again).
const KEEP: Duration = Duration::from_secs(300);

type GithubAnswer = Option<Result<GhOk, String>>;

struct Kept<T> {
    at: Instant,
    /// What the answer was about (a fingerprint of the token): another token is another question.
    about: String,
    value: T,
}

/// The wizard's state: the probes, and the slow answers kept for a few minutes.
#[derive(Default)]
pub struct Wizard {
    probes: OnceLock<Arc<dyn Probes>>,
    github: Mutex<Option<Kept<GithubAnswer>>>,
    claude: Mutex<Option<Kept<ClaudeStatus>>>,
}

impl Wizard {
    pub(super) fn forget(&self) {
        *crate::sync::lock(&self.github) = None;
        *crate::sync::lock(&self.claude) = None;
    }
}

fn fresh<T: Clone>(kept: &Mutex<Option<Kept<T>>>, about: &str) -> Option<T> {
    let held = crate::sync::lock(kept);
    held.as_ref()
        .filter(|k| k.about == about && k.at.elapsed() < KEEP)
        .map(|k| k.value.clone())
}

fn keep<T>(kept: &Mutex<Option<Kept<T>>>, about: &str, value: T) {
    *crate::sync::lock(kept) = Some(Kept {
        at: Instant::now(),
        about: about.to_string(),
        value,
    });
}

impl SettingsServer {
    /// Replaces what the wizard asks `gh` and `claude` (tests; the real one is used otherwise).
    pub fn set_wizard_probes(&self, probes: Arc<dyn Probes>) {
        let _ = self.wizard.probes.set(probes);
    }

    fn probes(&self) -> Arc<dyn Probes> {
        self.wizard
            .probes
            .get_or_init(|| Arc::new(RealProbes { env: self.env.clone() }))
            .clone()
    }

    /// Whether the wizard still has something to do, for the first-visit door (section 3.1): true until a visit's
    /// `GET /api/wizard` found every step done and noted it (`wizard.json` in the data folder), so a wiped data folder,
    /// which has no note, opens the door again and a set-up computer that restarts does not.
    pub(crate) fn wizard_incomplete(&self) -> bool {
        incomplete_in(&self.dir)
    }

    /// Notes whether every step was done, so the door opens or shuts (written only when it changes).
    fn note_complete(&self, complete: bool) {
        if self.wizard_incomplete() == !complete {
            return;
        }
        let note = json!({ "complete": complete });
        if let Err(e) = crate::fs_util::write_json_atomic(&self.dir.join(NOTE_FILE), &note, true) {
            tracing::warn!("couldn't note whether setup is complete: {e:#}");
        }
    }
}

/// `wizard_incomplete` for a data folder: the door's own question.
pub(super) fn incomplete_in(dir: &std::path::Path) -> bool {
    !std::fs::read_to_string(dir.join(NOTE_FILE))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .is_some_and(|v| v["complete"] == json!(true))
}

/// A JSON object of the changes a save sends to `Secrets::plan`.
fn one_secret(name: &str, value: &str) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert(name.to_string(), json!(value));
    m
}
