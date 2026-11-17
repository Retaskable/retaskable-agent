//! `promote` (`agent/src/promote.ts`, environments.md sections 4.2 and 4.3): the checks again, then `git push origin
//! <sha>:refs/heads/<production>` from the main checkout, the sha asked for and not whatever staging's head is by now.
//! Logged either way once it gets to GitHub.
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};

use serde_json::{Map, Value, json};

use super::{Environments, PUSH_TIMEOUT, Promoter, Stop, Via, environments_of, git, host, push_refusal, short};

/// Why Promote is refused for a project without both branches (`needEnvironments`).
pub(super) const NO_ENVIRONMENTS: &str = "This project has no staging and production branches: set them in Settings → Project settings, under Staging and Production.";

/// The repos with a push under way: one Promote at a time per repo, a second press is refused, not queued.
static PUSHING: LazyLock<Mutex<HashSet<PathBuf>>> = LazyLock::new(Mutex::default);

/// Holds a repo's place in [`PUSHING`] until dropped.
pub(super) struct Pushing(PathBuf);

impl Pushing {
    pub(super) fn take(repo: &std::path::Path) -> Option<Self> {
        let key = std::fs::canonicalize(repo).unwrap_or_else(|_| repo.to_path_buf());
        // Built only once the place is taken, and after the lock is let go: a guard made for a place that was refused
        // would free the holder's on drop.
        let taken = PUSHING.lock().unwrap().insert(key.clone());
        taken.then(|| Self(key))
    }
}

impl Drop for Pushing {
    fn drop(&mut self) {
        PUSHING.lock().unwrap().remove(&self.0);
    }
}

/// `7` to `40` hex digits (`/^[0-9a-f]{7,40}$/i`).
fn is_sha(s: &str) -> bool {
    (7..=40).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

impl Promoter {
    /// Section 4.2's first check, on git alone: `sha` is on staging and ahead of production. Its reason, if not.
    async fn git_check(&self, env: &Environments, sha: &str) -> Result<Option<String>, String> {
        let prod = format!("origin/{}", env.production.branch);
        let staging = format!("origin/{}", env.staging.branch);
        if !git::is_ancestor(&self.repo, sha, &staging).await? {
            return Ok(Some(format!("{} isn't on {}", short(sha), env.staging.branch)));
        }
        if git::is_ancestor(&self.repo, sha, &prod).await? {
            return Ok(Some("Up to date".into()));
        }
        if !git::is_ancestor(&self.repo, &prod, sha).await? {
            return Ok(Some("Production has commits staging doesn't: fix that by hand".into()));
        }
        Ok(None)
    }

    /// Section 4.2's second check, Retaskable's layer: only the repo's admins promote, asked of this computer's own
    /// `gh` login. Its reason, if not.
    async fn admin_check(&self) -> Option<String> {
        match host::permissions(&self.gh, &self.repo, &self.github_repo).await {
            Err(e) => Some(format!(
                "Couldn't ask GitHub whether you're an admin of {}: {e}",
                self.github_repo
            )),
            Ok((true, _)) => None,
            Ok((false, true)) => Some("Only the repo's admins can promote (you have write access)".into()),
            Ok((false, false)) => Some("Only the repo's admins can promote".into()),
        }
    }

    /// Promotes `sha` (seven to forty hex digits): the checks, one push, a refetch. The commit pushed, in full. The
    /// work runs on a task of its own, so an awaiter that gives up (a rollout that lost its runner) never leaves a
    /// push done and its log row unwritten.
    pub async fn promote(&self, sha: &str, via: Via) -> Result<String, Stop> {
        let (this, sha) = (self.clone(), sha.to_string());
        tokio::spawn(async move { this.promote_now(&sha, via).await })
            .await
            .map_err(|e| Stop::Failed(format!("the promote stopped: {e}")))?
    }

    async fn promote_now(&self, sha: &str, via: Via) -> Result<String, Stop> {
        let env = environments_of(&self.deploy).ok_or_else(|| Stop::refused(NO_ENVIRONMENTS))?;
        if !is_sha(sha) {
            return Err(Stop::refused("Promote needs the commit to push."));
        }
        let Some(_place) = Pushing::take(&self.repo) else {
            return Err(Stop::refused("A promote is already being pushed."));
        };
        git::fetch(&self.repo).await.map_err(Stop::Failed)?;
        let full = git::full_sha(&self.repo, sha).await.map_err(Stop::Failed)?;
        if full.is_empty() {
            return Err(Stop::refused(format!("{} isn't a commit in this repo", short(sha))));
        }
        if let Some(why) = self.git_check(&env, &full).await.map_err(Stop::Failed)? {
            return Err(Stop::refused(why));
        }
        if let Some(why) = self.admin_check().await {
            return Err(Stop::refused(why));
        }
        let from = git::rev(&self.repo, &format!("origin/{}", env.production.branch))
            .await
            .map_err(Stop::Failed)?;
        // From the website the log names whoever pressed it there, not the machine's login.
        let login = match &via {
            Via::Site(who) => who.clone(),
            _ => host::login(&self.gh, &self.repo).await.unwrap_or_default(),
        };
        let args = git::push_args(&full, &env.production.branch);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let pushed = git::run(&self.repo, &args, PUSH_TIMEOUT).await.map_err(Stop::Failed)?;
        let entry = self.entry(&login, &via, &from, &full);
        if !pushed.ok {
            let reason = if pushed.timed_out {
                "the push didn't finish in 5 minutes".to_string()
            } else {
                push_refusal(&if pushed.stderr.is_empty() {
                    pushed.stdout
                } else {
                    pushed.stderr
                })
            };
            self.log.add("deploy", with_outcome(entry, "refused", Some(&reason)));
            return Err(Stop::refused(format!("GitHub refused the push: {reason}")));
        }
        self.log.add("deploy", with_outcome(entry, "pushed", None));
        let _ = git::fetch(&self.repo).await;
        Ok(full)
    }

    /// The deploy log row, but for its outcome: `{ at, login, environment, from, to, via? }`.
    fn entry(&self, login: &str, via: &Via, from: &str, to: &str) -> Map<String, Value> {
        let at = chrono::DateTime::from_timestamp_millis((self.now_ms)() as i64)
            .unwrap_or_default()
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let mut m = Map::new();
        for (k, v) in [
            ("at", json!(at)),
            ("login", json!(login)),
            ("environment", json!("production")),
            ("from", json!(from)),
            ("to", json!(to)),
        ] {
            m.insert(k.into(), v);
        }
        match via {
            Via::Machine => {}
            Via::Auto => {
                m.insert("via".into(), json!("auto"));
            }
            Via::Site(_) => {
                m.insert("via".into(), json!("site"));
            }
        }
        m
    }
}

fn with_outcome(mut entry: Map<String, Value>, outcome: &str, reason: Option<&str>) -> Map<String, Value> {
    entry.insert("outcome".into(), json!(outcome));
    if let Some(r) = reason {
        entry.insert("reason".into(), json!(r));
    }
    entry
}
