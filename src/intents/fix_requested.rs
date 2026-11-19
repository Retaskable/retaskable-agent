//! The `fix_requested` kind (docs/specs/intent-rows.md section 4; the `ci_fix` command in the TS agent, `ciFixNow` in its
//! `ciWatch.ts`): the owner's **Fix** on a red pull request, `{ pr, sha }`. Only the computer the store names as the
//! project's runner takes the row (it is the one that watches the pull requests), and only when the project isn't paused
//! and Claude is signed in: the fix is a session. The handler asks the watcher to press: it checks the pull request is
//! open at exactly that head and asks the store, which counts the attempt. The row then ends `done` with `{ says }` at
//! once, and the fix goes on in the background; its attempt and verdict are on the pull request's log, not on the row.
//!
//! A refusal (a fix already running, not the runner, the head moved on, the store's own words) ends the row `refused`
//! with its reason. The TS agent throws a plain `Error` there, which its puller ends as `failed`; the spec says the agent
//! "refuses, with the reason shown", so it is `refused` here.
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use serde_json::json;

use super::{Handler, Outcome};
use crate::backlog::intents::IntentRow;
use crate::ci_pass::press::{NOT_WATCHING, Press, PressError};

/// What the watcher does for a press. Faked in tests.
pub trait FixPress: Send + Sync {
    /// Checks the press and starts the fix in the background; what to say at once, or why not.
    fn press(&self, pr: u64, sha: &str) -> BoxFuture<'static, Result<String, PressError>>;
}

/// The real one: [`Press::begin`], its background fix left to run on by itself.
pub struct Presser(pub Arc<Press>);

impl FixPress for Presser {
    fn press(&self, pr: u64, sha: &str) -> BoxFuture<'static, Result<String, PressError>> {
        let (press, sha) = (self.0.clone(), sha.to_string());
        async move { press.begin(pr, &sha).await.map(|started| started.says) }.boxed()
    }
}

/// The puller's context: the watcher of the project, when this computer has one (a project with no shared backlog or no
/// rollout run has none, and the row is refused).
pub type FixCx = Option<Arc<dyn FixPress>>;

pub struct FixHandler {
    /// Whether the store last named this computer the project's runner: the one that watches pull requests.
    runner: Arc<AtomicBool>,
}

impl FixHandler {
    pub fn new(runner: Arc<AtomicBool>) -> Self {
        Self { runner }
    }
}

/// `{ pr, sha }` as the command's schema has them: a positive whole number and a 7 to 64 character hex commit.
fn args_of(row: &IntentRow) -> Result<(u64, &str), String> {
    let pr = row.args.get("pr").and_then(serde_json::Value::as_u64).filter(|&n| n > 0);
    let sha = (row.args.get("sha").and_then(|v| v.as_str()))
        .filter(|s| (7..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit()));
    match (pr, sha) {
        (Some(pr), Some(sha)) => Ok((pr, sha)),
        (None, _) => Err("pr: expected a positive whole number".into()),
        (_, None) => Err("sha: expected a commit of 7 to 64 hex characters".into()),
    }
}

impl Handler<FixCx> for FixHandler {
    fn kind(&self) -> &'static str {
        "fix_requested"
    }

    /// Only the runner: another computer leaves the row open instead of claiming it to refuse it.
    fn takes(&self, _row: &IntentRow) -> bool {
        self.runner.load(Ordering::SeqCst)
    }

    fn starts_work(&self) -> bool {
        true
    }

    fn runs_session(&self) -> bool {
        true
    }

    fn run(&self, row: IntentRow, cx: &FixCx) -> BoxFuture<'static, Outcome> {
        let watcher = cx.clone();
        async move {
            let (pr, sha) = match args_of(&row) {
                Ok(args) => args,
                Err(why) => return Outcome::Refused(why),
            };
            let Some(watcher) = watcher else {
                return Outcome::Refused(NOT_WATCHING.into());
            };
            match watcher.press(pr, sha).await {
                Ok(says) => Outcome::Done(json!({ "says": says })),
                Err(PressError::Refused(why)) => Outcome::Refused(why),
                Err(PressError::Failed(why)) => Outcome::Failed(why),
            }
        }
        .boxed()
    }
}

#[cfg(test)]
mod tests;
