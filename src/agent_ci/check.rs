//! docs/specs/agent-ci.md sections 3.2 and 3.3: one claimed check end to end. The caller holds the slot's permit
//! for the whole of it: pending, fetch (`host.rs`), a conflict fails it with no container, else run
//! (`container.rs`), then success or failure (and on failure the comment), then the directory goes.
use crate::text::is_blank;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use serde_json::json;

use super::automerge::{self, Automerge};
use super::claim;
use super::prs::PrMeta;
use super::report::{self, Host, Kind, State};
use super::settings::Repo;
use super::store::{self, Claim, Claimed};
use super::{CheckKey, Programs, container, host};
use crate::backlog::runs::short_sha;
use crate::telemetry::EventLog;
use crate::telemetry::event::{Ci, Event};

/// Everything one check needs.
pub struct Job {
    pub programs: Programs,
    pub data_dir: PathBuf,
    pub computer: String,
    pub repo: Repo,
    /// The repo's bare mirror: where `gh` runs and the fetch step clones from.
    pub mirror: PathBuf,
    /// `--cpus` of the container, already capped at what Docker has.
    pub cpus: u32,
    pub image: String,
    pub key: CheckKey,
    /// The store (the lease, the run record) and what merges a green pull request.
    pub merge: Automerge,
    /// The pull request's branch, title and author, as a claim and a run record tell the store.
    pub meta: PrMeta,
    /// A commit named on the command line (`once`): checked whatever its status says, and the claim forced.
    pub force: bool,
    pub clock: super::Clock,
    /// How often a running check sends its heartbeat.
    pub refresh: std::time::Duration,
    /// The pause between `ci_finish` tries.
    pub finish_pause: std::time::Duration,
}

/// `ci_finish` is tried this many times.
const FINISH_TRIES: u32 = 3;

/// How a poll's attempt at a key ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ran {
    /// Claimed and run, or finished by a computer already: not tried again.
    Done,
    /// Another computer's claim comes first: look again at the next poll.
    Lost,
}

/// The status context the key's check posts under.
fn context_of(k: &CheckKey) -> &'static str {
    if k.is_push() {
        report::PUSH_CONTEXT
    } else {
        report::PR_CONTEXT
    }
}

/// `<head>-<main>`, short: the key as the failure comment and events carry it.
pub fn key_text(k: &CheckKey) -> String {
    if k.is_push() {
        return short_sha(&k.main).to_string();
    }
    format!("{}-{}", short_sha(&k.head), short_sha(&k.main))
}

/// Writes a `ci` event to the project's log.
fn emit(j: &Job, state: &str, ms: u128) {
    let mut log = EventLog::new();
    log.open(&crate::paths::project_dir(&j.data_dir, &j.repo.id()).join("events"));
    let mut ci = Ci {
        item: Some(if j.key.is_push() {
            "push main".into()
        } else {
            format!("PR #{}", j.key.pr)
        }),
        state: Some(state.into()),
        ms: Some(ms as f64),
        ..Default::default()
    };
    ci.extra.insert("key".into(), json!(key_text(&j.key)));
    ci.extra.insert("pr".into(), json!(j.key.pr));
    log.add_in(&j.repo.id(), Event::Ci(ci));
}

/// What a log line calls a check: `PR #7 abcdef1` or `push abcdef1`.
fn subject(k: &CheckKey) -> String {
    if k.is_push() {
        format!("push {}", short_sha(&k.head))
    } else {
        format!("PR #{} {}", k.pr, short_sha(&k.head))
    }
}

/// The log's last non-empty line, for the failure status.
fn last_line(tail: &str) -> &str {
    tail.lines().rev().find(|l| !is_blank(l)).unwrap_or("")
}

/// The check's directory has a sibling file naming its container, so a restart can remove a cut one.
fn name_file(dir: &Path) -> PathBuf {
    dir.with_extension("name")
}

/// The event a check runs for, as the runner names it.
fn event_of(k: &CheckKey) -> &'static str {
    if k.is_push() { "push" } else { "pull_request" }
}

/// Whether the commit's newest status of the check's context leaves the run to this computer (`claim::wants_run`,
/// asked as under an agent: the store's lease decides between computers). A host that can't be read is an error.
async fn wanted(j: &Job, host_api: &Arc<Host>, context: &str) -> anyhow::Result<bool> {
    let (h, head) = (host_api.clone(), j.key.head.clone());
    let all = tokio::task::spawn_blocking(move || h.read_statuses(&head)).await??;
    let me = report::me(&j.computer);
    Ok(claim::wants_run(claim::newest(&all, context), &me, (j.clock)(), true))
}

/// The lease: `ci_claim` to the store. A store that can't be reached is an error (nothing runs).
async fn lease(j: &Job, context: &str) -> Result<Claimed, String> {
    let c = Claim {
        sha: &j.key.head,
        context,
        event: event_of(&j.key),
        pr: j.key.pr,
        branch: &j.meta.branch,
        title: &j.meta.title,
        author: &j.meta.author,
        computer: &j.computer,
    };
    let mut args = store::claim_args(&c);
    if j.force {
        args["force"] = json!(true);
    }
    let v = j.merge.store().op(&j.repo.github_repo, "ci_claim", args).await?;
    Ok(store::read_claim(&v))
}

/// Claims the key and, if this computer wins, runs the check; the last status is posted before this returns.
/// A commit whose status leaves no run to this computer is done at once. A key that can't be claimed (the store or
/// the host unreachable, or another computer holds the lease) is tried again next poll. Failures to reach the host
/// after the claim are logged: the check still counts as finished, so a broken `gh` doesn't rerun the container
/// every poll.
pub async fn run(j: Job) -> Ran {
    let host_api = Arc::new(Host {
        program: PathBuf::from(&j.programs.gh),
        kind: Kind::Github,
        repo: j.repo.github_repo.clone(),
        dir: j.mirror.clone(),
    });
    let key = key_text(&j.key);
    let context = context_of(&j.key);
    if !j.force {
        match wanted(&j, &host_api, context).await {
            Ok(true) => {}
            Ok(false) => return Ran::Done,
            Err(e) => {
                tracing::warn!("agent CI: reading the statuses of {key} failed: {e:#}");
                return Ran::Lost;
            }
        }
    }
    match lease(&j, context).await {
        Ok(Claimed::Won) => emit(&j, "claimed", 0),
        Ok(Claimed::Finished) => return Ran::Done,
        Ok(Claimed::Held(_)) => return Ran::Lost,
        Err(e) => {
            tracing::warn!("agent CI: claiming {key} failed: {e}");
            return Ran::Lost;
        }
    }
    let begun = Instant::now();
    tracing::info!("agent CI: {} {}: checking", j.repo.github_repo, subject(&j.key));
    emit(&j, "started", 0);
    let ok = go(&j, begun, host_api.clone()).await;
    let ms = begun.elapsed().as_millis();
    emit(&j, "finished", ms);
    let verdict = if ok { "passed" } else { "failed" };
    tracing::info!(
        "agent CI: {} {}: {verdict} in {}s",
        j.repo.github_repo,
        subject(&j.key),
        ms / 1000
    );
    finish(&j, ok, ms).await;
    if ok {
        land(&j, &host_api).await;
    }
    Ran::Done
}

/// The run's result goes in the project's history (`ci_finish`); the status is already the truth, so a store that
/// can't be reached only loses the record, after three tries.
async fn finish(j: &Job, ok: bool, ms: u128) {
    let context = context_of(&j.key);
    let mut args = json!({
        "sha": j.key.head,
        "context": context,
        "event": event_of(&j.key),
        "result": if ok { "passed" } else { "failed" },
        "minutes": (ms as f64 / 6000.0).round() / 10.0,
        "computer": j.computer,
    });
    if !j.key.is_push() {
        args["pr"] = json!(j.key.pr);
        for (k, v) in [
            ("branch", &j.meta.branch),
            ("title", &j.meta.title),
            ("author", &j.meta.author),
        ] {
            if !v.is_empty() {
                args[k] = json!(v);
            }
        }
    }
    for _ in 0..FINISH_TRIES {
        match j.merge.store().op(&j.repo.github_repo, "ci_finish", args.clone()).await {
            Ok(_) => return,
            Err(e) => {
                tracing::warn!("agent CI: ci_finish: {e}");
                tokio::time::sleep(j.finish_pause).await;
            }
        }
    }
}

async fn go(j: &Job, begun: Instant, host_api: Arc<Host>) -> bool {
    let me = report::me(&j.computer);
    let repo = j.mirror.clone();
    let head = j.key.head.clone();
    let minutes = || begun.elapsed().as_secs() / 60;
    let context = context_of(&j.key);
    let post = |state: State, text: String| {
        let (h, head) = (host_api.clone(), head.clone());
        async move {
            let r = tokio::task::spawn_blocking(move || h.post_status(&head, context, state, &text, None)).await;
            match r {
                Ok(Err(e)) => tracing::warn!("agent CI: posting a status failed: {e:#}"),
                Err(e) => tracing::warn!("agent CI: posting a status failed: {e}"),
                Ok(Ok(())) => {}
            }
        }
    };
    // The pending status is the display of the lease; it is posted now, and with each heartbeat to the store
    // until the last status.
    post(State::Pending, report::running_text(&me)).await;
    let beat = || {
        let (store, repo) = (j.merge.store().clone(), j.repo.github_repo.clone());
        let args = json!({ "sha": j.key.head, "context": context });
        let pending = post(State::Pending, report::running_text(&me));
        async move {
            match store.op(&repo, "ci_heartbeat", args).await {
                Ok(v) if store::lease_lost(&v) => tracing::warn!("agent CI: lease lost"),
                Err(e) => tracing::warn!("agent CI: heartbeat: {e}"),
                Ok(_) => {}
            }
            pending.await;
        }
    };
    let fail = |reason: String, log: String| {
        let (h, key, head) = (host_api.clone(), key_text(&j.key), j.key.head.clone());
        let pr = j.key.pr;
        let text = report::failed_text(&me, minutes(), &reason);
        let post_status = post(State::Failure, text);
        async move {
            post_status.await;
            // A push to main has no pull request to comment on.
            if pr == 0 {
                return;
            }
            let _ = tokio::task::spawn_blocking(move || {
                if let Err(e) = h.post_failure_comment(pr, "agent CI", &head, &key, &log) {
                    tracing::warn!("agent CI: posting the failure comment failed: {e:#}");
                }
            })
            .await;
        }
    };

    // Only the fetch and the run are refreshed: the final status and the comment are posted after the last
    // refresh, so a late refresh can't turn the finished check pending again.
    let result: Result<(), (String, String)> = claim::refreshing(j.refresh, beat, async {
        let (data, id, main) = (j.data_dir.clone(), j.repo.id(), j.repo.base_branch.clone());
        let head_ref = format!("refs/pull/{}/head", j.key.pr);
        let push = j.key.is_push();
        let fetched = tokio::task::spawn_blocking(move || {
            host::fetch(
                &data,
                &id,
                &repo,
                &main,
                if push {
                    host::Mode::Push
                } else {
                    host::Mode::PullRequest { head_ref: &head_ref }
                },
            )
        })
        .await;
        let dir = match fetched {
            Ok(Ok(host::FetchOutcome::Ready { dir, .. })) => dir,
            Ok(Ok(host::FetchOutcome::Conflict { reason })) => {
                let line = format!("conflicts with {}: {}", j.repo.base_branch, last_line(&reason));
                return Err((line, reason));
            }
            other => {
                let why = match other {
                    Ok(Err(e)) => format!("{e:#}"),
                    Err(e) => e.to_string(),
                    Ok(Ok(_)) => unreachable!(),
                };
                tracing::warn!("agent CI: fetch failed: {why}");
                return Err((format!("fetch failed: {}", last_line(&why)), why));
            }
        };

        let name = container::container_name(&j.repo.github_repo, &j.key.head);
        let _ = std::fs::write(name_file(&dir), &name);
        let log_dir = crate::paths::project_dir(&j.data_dir, &j.repo.id()).join("ci-logs");
        let outcome = container::run(&container::Check {
            docker: &j.programs.docker,
            repo: &j.repo.github_repo,
            sha: &j.key.head,
            dir: &dir,
            image: &j.image,
            mode: if push {
                container::Mode::Push
            } else {
                container::Mode::PullRequest
            },
            log_dir: &log_dir,
            ceiling: container::CEILING,
            cpus: j.cpus,
        })
        .await;
        host::cleanup(&dir);
        let _ = std::fs::remove_file(name_file(&dir));
        match outcome {
            Ok(o) if o.exit_code == Some(0) => Ok(()),
            Ok(o) => {
                let reason = if o.timed_out {
                    "timed out".to_string()
                } else {
                    last_line(&o.tail).to_string()
                };
                Err((reason, o.tail))
            }
            Err(e) => Err((format!("couldn't run: {e:#}"), format!("{e:#}"))),
        }
    })
    .await;
    match result {
        Ok(()) => {
            post(State::Success, report::passed_text(&me, minutes())).await;
            true
        }
        Err((reason, log)) => {
            fail(reason, log).await;
            false
        }
    }
}

/// A green check's `automerge`: the pull request is squash-merged pinned to its head, or says why not.
async fn land(j: &Job, host_api: &Arc<Host>) {
    if j.key.is_push() {
        return;
    }
    let ended = automerge::Automerge::run(&j.merge, host_api, &j.repo.base_branch, j.key.pr, &j.key.head).await;
    match ended {
        automerge::Ended::Merged => tracing::info!("agent CI: merged PR #{}", j.key.pr),
        automerge::Ended::Held(_) | automerge::Ended::NotAsked => {}
        automerge::Ended::Failed(why) => tracing::warn!("agent CI: merging PR #{} failed: {why}", j.key.pr),
    }
}

/// Recovery (agent.md section 7.4), at start: removes the container each leftover check named, then every
/// leftover directory.
pub async fn recover(docker: &std::ffi::OsStr, data_dir: &Path, project: &str) {
    let root = crate::paths::ci_dir(data_dir, project);
    if let Ok(entries) = std::fs::read_dir(&root) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "name")
                && let Ok(name) = std::fs::read_to_string(&p)
                && !is_blank(&name)
            {
                container::remove_stale(docker, name.trim()).await;
            }
        }
    }
    host::sweep(data_dir, project);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr_key() -> CheckKey {
        CheckKey {
            pr: 7,
            head: "abcdef1234567".into(),
            main: "0123456789abc".into(),
        }
    }

    #[test]
    fn a_pull_request_key_text_is_short_head_then_short_main() {
        assert_eq!(key_text(&pr_key()), "abcdef1-0123456");
    }

    #[test]
    fn a_push_key_text_is_main_alone_short() {
        assert_eq!(key_text(&CheckKey::push("0123456789abc")), "0123456");
    }

    #[test]
    fn the_status_context_depends_on_push_or_pr() {
        assert_eq!(context_of(&pr_key()), report::PR_CONTEXT);
        assert_eq!(context_of(&CheckKey::push("abc")), report::PUSH_CONTEXT);
    }

    #[test]
    fn last_line_skips_trailing_blank_lines() {
        assert_eq!(last_line("one\ntwo\n\n  \n"), "two");
        assert_eq!(last_line("only"), "only");
        assert_eq!(last_line(""), "");
        assert_eq!(last_line("\n \n"), "");
    }

    #[test]
    fn the_name_file_is_a_sibling_of_the_check_directory() {
        let dir = Path::new("root").join("check-1");
        assert_eq!(name_file(&dir), Path::new("root").join("check-1.name"));
    }
}
