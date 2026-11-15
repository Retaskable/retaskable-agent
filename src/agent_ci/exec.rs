//! docs/specs/agent-ci.md section 3.2: the containers of one check in order, as `runner.mjs` runs them: the host
//! reads the token, the changed files and the card line; the fetch container brings the change into the repo's
//! mirror volume and names the image; the warm-up fills the dependency volume (default image and a `warm.sh`
//! only); the check runs. Everything goes to the check's log, which is also where a step's failure is told.
use std::ffi::OsStr;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use super::container::{self, CEILING, Step as Run};
use super::report::Host;
use super::steps::{self, DEFAULT_IMAGE, Plan, Step};

pub struct Exec<'a> {
    pub docker: &'a OsStr,
    pub host: &'a Arc<Host>,
    pub plan: &'a Plan<'a>,
    pub log: &'a Path,
    /// The pull request's branch (empty for a push).
    pub branch: &'a str,
    /// Whether main has `.agent-ci/warm.sh`.
    pub has_warm: bool,
    /// When the check began: the steps share one ceiling.
    pub begun: Instant,
}

/// Puts a line in the log, as the runner does for what a step can't say itself.
fn say(log: &Path, line: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new().append(true).create(true).open(log) {
        let _ = writeln!(f, "{line}");
    }
}

impl Exec<'_> {
    async fn step(&self, step: Step, args: Vec<String>, token: Option<&str>) -> Option<i32> {
        let name = self.plan.name(step);
        let ceiling = CEILING.saturating_sub(self.begun.elapsed());
        let run = Run {
            docker: self.docker,
            name: &name,
            args,
            token,
            log: self.log,
            ceiling,
        };
        match container::run(&run).await {
            Ok(e) if e.timed_out => {
                say(self.log, "error: timed out");
                None
            }
            Ok(e) => e.exit_code,
            Err(err) => {
                say(self.log, &format!("error: couldn't run: {err:#}"));
                None
            }
        }
    }

    /// Fetch, warm and check; whether the check passed. The log says why not.
    pub async fn containers(&self) -> bool {
        let (h, pr, branch) = (self.host.clone(), self.plan.pr, self.branch.to_string());
        let push = self.plan.event == "push";
        let read = tokio::task::spawn_blocking(move || {
            let files = if push { "[]".to_string() } else { h.pr_files(pr) };
            (h.token(), files, h.card_line(pr, &branch))
        })
        .await;
        let Ok((token, files, card)) = read else {
            say(self.log, "error: couldn't read the pull request");
            return false;
        };
        let token = match token {
            Ok(t) => t,
            Err(e) => {
                say(self.log, &format!("error: {e:#}"));
                return false;
            }
        };
        if self.step(Step::Fetch, self.plan.fetch_args(), Some(&token)).await != Some(0) {
            return false;
        }
        // The image is read from what the fetch printed; a bad name fails the run before any docker call uses it.
        let image = match steps::image_from(&container::read_log(self.log)) {
            Ok(i) => i,
            Err(e) => {
                say(self.log, &format!("error: {e}"));
                return false;
            }
        };
        // The warm-up is for the default image's dependency volume; a failed one only costs the checks their install.
        if image == DEFAULT_IMAGE && self.has_warm {
            self.step(Step::Warm, self.plan.warm_args(), None).await;
        }
        let args = self.plan.check_args(&image, &files, card.as_deref());
        self.step(Step::Check, args, None).await == Some(0)
    }
}
