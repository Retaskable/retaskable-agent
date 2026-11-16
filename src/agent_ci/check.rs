//! docs/specs/agent-ci.md sections 3.2 and 3.3: one claimed check end to end. The caller holds the slot's permit
//! for the whole of it: pending, the three containers (`exec.rs`), then the failure's comment and the last status.
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use serde_json::json;

use super::automerge::{self, Automerge};
use super::claim;
use super::exec::Exec;
use super::prs::PrMeta;
use super::report::{self, Host, Kind, State};
use super::settings::Repo;
use super::steps::{self, Plan};
use super::store::{self, CiStore, Claim, Claimed};
use super::{CheckKey, Programs, container};
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
    /// The runner's slots (`AGENT_CI_SLOTS`), told to the store with the lease for the Agents tab.
    pub slots: u32,
    /// Whether main has `.agent-ci/warm.sh`.
    pub has_warm: bool,
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

/// The log's last line that looks like a failure, for the failure status (the runner's `/not ok|error|fail|✖/i`).
fn last_line(log: &str) -> &str {
    static R: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = R.get_or_init(|| regex::Regex::new(r"(?i)not ok|error|fail|✖").expect("a fixed pattern"));
    log.trim_end().lines().rev().find(|l| re.is_match(l)).unwrap_or("")
}

/// The runner's minutes: tenths, as text (`1.5`).
fn minutes_text(ms: u128) -> String {
    format!("{:.1}", ms as f64 / 60000.0)
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
        slots: j.slots,
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
    let done = go(&j, begun, host_api.clone()).await;
    let (ok, ms) = (done.ok, begun.elapsed().as_millis());
    emit(&j, "finished", ms);
    let verdict = if ok { "passed" } else { "failed" };
    tracing::info!(
        "agent CI: {} {}: {verdict} in {}s",
        j.repo.github_repo,
        subject(&j.key),
        ms / 1000
    );
    finish(&j, &done).await;
    if ok {
        land(&j, &host_api).await;
    }
    Ran::Done
}

/// The run's result goes in the project's history (`ci_finish`); the status is already the truth, so a store that
/// can't be reached after three tries leaves the record to [`keep_finishing`].
async fn finish(j: &Job, done: &Done) {
    let context = context_of(&j.key);
    let mut args = json!({
        "sha": j.key.head,
        "context": context,
        "event": event_of(&j.key),
        "result": if done.ok { "passed" } else { "failed" },
        "minutes": done.minutes.parse::<f64>().unwrap_or(0.0),
        "computer": j.computer,
    });
    if let Some(link) = &done.link {
        args["link"] = json!(link);
    }
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
    let (store, repo) = (j.merge.store().clone(), j.repo.github_repo.clone());
    if !try_finish(&store, &repo, &args, j.finish_pause).await {
        // The check's own work is done (and `land` goes on), so the record is kept and sent from the side, not
        // held up in this slot.
        tokio::spawn(keep_finishing(store, repo, args));
    }
}

/// `ci_finish`, tried [`FINISH_TRIES`] times `pause` apart. Whether the store took it.
async fn try_finish(
    store: &Arc<dyn CiStore>,
    repo: &str,
    args: &serde_json::Value,
    pause: std::time::Duration,
) -> bool {
    for _ in 0..FINISH_TRIES {
        match store.op(repo, "ci_finish", args.clone()).await {
            Ok(_) => return true,
            Err(e) => {
                tracing::warn!("agent CI: ci_finish: {e}");
                tokio::time::sleep(pause).await;
            }
        }
    }
    false
}

/// How often a `ci_finish` the store didn't take is sent again, and for how long.
const KEEP_EVERY: std::time::Duration = std::time::Duration::from_secs(30);
const KEPT: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// Sends the run record again every [`KEEP_EVERY`] until the store answers or [`KEPT`] passes (the owner's outage rule,
/// 2026-11-16: "it should be able to finish what it's doing but it can't pick up the next thing till the store is
/// back."). The store takes a `ci_finish` from the computer whose lease lapsed while nobody else took the check. An
/// improvement over the TS runner, which tries three times and loses the record. In memory: a restart forgets it, as
/// the next runner's check would record the commit anyway.
async fn keep_finishing(store: Arc<dyn CiStore>, repo: String, args: serde_json::Value) {
    let mut waited = std::time::Duration::ZERO;
    while waited < KEPT {
        tokio::time::sleep(KEEP_EVERY).await;
        waited += KEEP_EVERY;
        if store.op(&repo, "ci_finish", args.clone()).await.is_ok() {
            return;
        }
    }
    tracing::error!("agent CI: ci_finish never reached the store in 24 hours; giving it up");
}

/// How a check ended: its verdict, its minutes as the runner words them, and the failure comment's link.
struct Done {
    ok: bool,
    minutes: String,
    link: Option<String>,
}

async fn go(j: &Job, begun: Instant, host_api: Arc<Host>) -> Done {
    let me = report::me(&j.computer);
    let head = j.key.head.clone();
    let context = context_of(&j.key);
    let post = |state: State, text: String, link: Option<String>| {
        let (h, head) = (host_api.clone(), head.clone());
        async move {
            let r =
                tokio::task::spawn_blocking(move || h.post_status(&head, context, state, &text, link.as_deref())).await;
            match r {
                Ok(Err(e)) => tracing::warn!("agent CI: posting a status failed: {e:#}"),
                Err(e) => tracing::warn!("agent CI: posting a status failed: {e}"),
                Ok(Ok(())) => {}
            }
        }
    };
    // The pending status is the display of the lease; it is posted now, and with each heartbeat to the store
    // until the last status.
    post(State::Pending, report::running_text(&me), None).await;
    let beat = || {
        let (store, repo) = (j.merge.store().clone(), j.repo.github_repo.clone());
        let args = store::heartbeat_args(&j.key.head, context, j.slots);
        let pending = post(State::Pending, report::running_text(&me), None);
        async move {
            match store.op(&repo, "ci_heartbeat", args).await {
                Ok(v) if store::lease_lost(&v) => tracing::warn!("agent CI: lease lost"),
                Err(e) => tracing::warn!("agent CI: heartbeat: {e}"),
                Ok(_) => {}
            }
            pending.await;
        }
    };

    let plan = Plan {
        computer: &j.computer,
        repo: &j.repo.github_repo,
        sha: &j.key.head,
        event: event_of(&j.key),
        pr: j.key.pr,
        cpus: j.cpus,
    };
    let log_dir = crate::paths::project_dir(&j.data_dir, &j.repo.id()).join("ci-logs");
    let header = format!(
        "{} {me} {} {} {}",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        plan.event,
        plan.ref_name(),
        j.key.head
    );
    let log = container::start_log(&log_dir, &j.key.head, plan.event, &header);
    // Only the containers are refreshed: the final status and the comment are posted after the last refresh, so
    // a late refresh can't turn the finished check pending again.
    let ok = match &log {
        Ok(log) => {
            let exec = Exec {
                docker: &j.programs.docker,
                host: &host_api,
                plan: &plan,
                log,
                branch: &j.meta.branch,
                has_warm: j.has_warm,
                begun,
            };
            claim::refreshing(j.refresh, beat, exec.containers()).await
        }
        Err(e) => {
            tracing::warn!("agent CI: {e:#}");
            false
        }
    };
    let minutes = minutes_text(begun.elapsed().as_millis());
    if ok {
        post(State::Success, report::passed_text(&me, &minutes), None).await;
        return Done {
            ok,
            minutes,
            link: None,
        };
    }
    let text = match &log {
        Ok(path) => container::read_log(path),
        Err(e) => format!("error: {e:#}"),
    };
    let tail = last_line(&text).to_string();
    // The failing lines and the log's end go in a comment the status links to.
    let link = {
        let (h, me, head, pr, text) = (host_api.clone(), me.clone(), head.clone(), j.key.pr, text);
        let posted = tokio::task::spawn_blocking(move || h.post_log_comment(&me, pr, &head, &text)).await;
        match posted {
            Ok(Ok(link)) => link,
            Ok(Err(e)) => {
                tracing::warn!("agent CI: posting the failure comment failed: {e:#}");
                None
            }
            Err(e) => {
                tracing::warn!("agent CI: posting the failure comment failed: {e}");
                None
            }
        }
    };
    post(State::Failure, report::failed_text(&me, &minutes, &tail), link.clone()).await;
    Done { ok, minutes, link }
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

/// Recovery (agent.md section 7.4), at start: removes this computer's leftover step containers of the repo.
pub async fn recover(docker: &std::ffi::OsStr, computer: &str, repo: &str) {
    container::remove_leftovers(docker, &steps::name_prefix(computer, repo)).await;
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
    fn the_status_tail_is_the_last_failure_looking_line() {
        assert_eq!(last_line("one\nnot ok 3 - a\nok 4\n\n  \n"), "not ok 3 - a");
        assert_eq!(last_line("fine\nERROR: x\nFailed: y\nok\n"), "Failed: y");
        assert_eq!(last_line("only fine"), "");
        assert_eq!(last_line(""), "");
    }

    #[test]
    fn minutes_are_tenths_as_text() {
        assert_eq!(minutes_text(0), "0.0");
        assert_eq!(minutes_text(90_000), "1.5");
        assert_eq!(minutes_text(125_000), "2.1");
    }
}

#[cfg(test)]
mod finish_tests;
