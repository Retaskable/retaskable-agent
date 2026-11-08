//! docs/specs/agent-ci.md sections 3.2 and 3.3: one claimed check end to end. The caller holds the slot's permit
//! for the whole of it: pending, fetch (`host.rs`), a conflict fails it with no container, else run
//! (`container.rs`), then success or failure (and on failure the comment), then the directory goes.
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::json;

use super::report::{self, Host, Kind, State};
use super::{CheckKey, Programs, RunningCheck, container, host};
use crate::config::ProjectConfig;
use crate::orchestrator::live::LiveHandle;
use crate::telemetry::event::{Ci, Event};
use crate::telemetry::{EventLog, now_ms};

/// Everything one check needs.
pub struct Job {
    pub programs: Programs,
    pub data_dir: PathBuf,
    pub computer: String,
    pub live: LiveHandle,
    pub id: String,
    pub project: ProjectConfig,
    pub image: String,
    pub key: CheckKey,
}

fn short(sha: &str) -> String {
    sha.chars().take(7).collect()
}

/// `<head>-<main>`, short: the key as the failure comment and events carry it.
pub fn key_text(k: &CheckKey) -> String {
    format!("{}-{}", short(&k.head), short(&k.main))
}

/// Writes a `ci` event to the project's log.
fn emit(j: &Job, state: &str, ms: u128) {
    let mut log = EventLog::new();
    log.open(&crate::paths::project_dir(&j.data_dir, &j.id).join("events"));
    let mut ci = Ci {
        item: Some(format!("PR #{}", j.key.pr)),
        state: Some(state.into()),
        ms: Some(ms as f64),
        ..Default::default()
    };
    ci.extra.insert("key".into(), json!(key_text(&j.key)));
    ci.extra.insert("pr".into(), json!(j.key.pr));
    log.add_in(&j.id, Event::Ci(ci));
}

/// The log's last non-empty line, for the failure status.
fn last_line(tail: &str) -> &str {
    tail.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("")
}

/// The check's directory has a sibling file naming its container, so a restart can remove a cut one.
fn name_file(dir: &Path) -> PathBuf {
    dir.with_extension("name")
}

/// Runs the check; the last status is posted before this returns. Failures to reach the host are logged: the
/// check still counts as finished, so a broken `gh` doesn't rerun the container every poll.
pub async fn run(j: Job) {
    let begun = Instant::now();
    let started_ms = now_ms();
    let sha7 = short(&j.key.head);
    j.live.update_ci(|c| {
        c.checks.push(RunningCheck {
            project: j.id.clone(),
            pr: j.key.pr,
            sha: sha7.clone(),
            started: begun,
        });
        c.running = c.checks.len() as u32;
    });
    emit(&j, "started", 0);
    let ok = go(&j, begun).await;
    let ms = begun.elapsed().as_millis();
    emit(&j, "finished", ms);
    j.live.update_ci(|c| {
        c.checks
            .retain(|r| !(r.project == j.id && r.pr == j.key.pr && r.sha == sha7));
        c.running = c.checks.len() as u32;
        c.last_result = Some(json!({ "project": j.id, "pr": j.key.pr, "sha": sha7, "ok": ok, "at": started_ms }));
    });
}

async fn go(j: &Job, begun: Instant) -> bool {
    let me = report::me(&j.computer);
    let repo = PathBuf::from(&j.project.repo_path);
    let host_api = std::sync::Arc::new(Host {
        program: PathBuf::from(&j.programs.gh),
        kind: Kind::Github,
        repo: j.project.github_repo.clone(),
        dir: repo.clone(),
    });
    let head = j.key.head.clone();
    let minutes = || begun.elapsed().as_secs() / 60;
    let post = |state: State, text: String| {
        let (h, head) = (host_api.clone(), head.clone());
        async move {
            let r =
                tokio::task::spawn_blocking(move || h.post_status(&head, report::PR_CONTEXT, state, &text, None)).await;
            match r {
                Ok(Err(e)) => tracing::warn!("agent CI: posting a status failed: {e:#}"),
                Err(e) => tracing::warn!("agent CI: posting a status failed: {e}"),
                Ok(Ok(())) => {}
            }
        }
    };
    post(State::Pending, report::running_text(&me)).await;
    let fail = |reason: String, log: String| {
        let (h, key, head) = (host_api.clone(), key_text(&j.key), j.key.head.clone());
        let pr = j.key.pr;
        let text = report::failed_text(&me, minutes(), &reason);
        let post_status = post(State::Failure, text);
        async move {
            post_status.await;
            let _ = tokio::task::spawn_blocking(move || {
                if let Err(e) = h.post_failure_comment(pr, "agent CI", &head, &key, &log) {
                    tracing::warn!("agent CI: posting the failure comment failed: {e:#}");
                }
            })
            .await;
        }
    };

    let (data, id, main) = (j.data_dir.clone(), j.id.clone(), j.project.base_branch.clone());
    let head_ref = format!("refs/pull/{}/head", j.key.pr);
    let fetched = tokio::task::spawn_blocking(move || {
        host::fetch(
            &data,
            &id,
            &repo,
            &main,
            host::Mode::PullRequest { head_ref: &head_ref },
        )
    })
    .await;
    let dir = match fetched {
        Ok(Ok(host::FetchOutcome::Ready { dir, .. })) => dir,
        Ok(Ok(host::FetchOutcome::Conflict { reason })) => {
            let line = format!("conflicts with {}: {}", j.project.base_branch, last_line(&reason));
            fail(line, reason).await;
            return false;
        }
        other => {
            let why = match other {
                Ok(Err(e)) => format!("{e:#}"),
                Err(e) => e.to_string(),
                Ok(Ok(_)) => unreachable!(),
            };
            tracing::warn!("agent CI: fetch failed: {why}");
            fail(format!("fetch failed: {}", last_line(&why)), why).await;
            return false;
        }
    };

    let name = container::container_name(&j.project.github_repo, &j.key.head);
    let _ = std::fs::write(name_file(&dir), &name);
    let log_dir = crate::paths::project_dir(&j.data_dir, &j.id).join("ci-logs");
    let outcome = container::run(&container::Check {
        docker: &j.programs.docker,
        repo: &j.project.github_repo,
        sha: &j.key.head,
        dir: &dir,
        image: &j.image,
        mode: container::Mode::PullRequest,
        log_dir: &log_dir,
        ceiling: container::CEILING,
    })
    .await;
    host::cleanup(&dir);
    let _ = std::fs::remove_file(name_file(&dir));
    match outcome {
        Ok(o) if o.exit_code == Some(0) => {
            post(State::Success, report::passed_text(&me, minutes())).await;
            true
        }
        Ok(o) => {
            let reason = if o.timed_out {
                "timed out".to_string()
            } else {
                last_line(&o.tail).to_string()
            };
            fail(reason, o.tail).await;
            false
        }
        Err(e) => {
            fail(format!("couldn't run: {e:#}"), format!("{e:#}")).await;
            false
        }
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
                && !name.trim().is_empty()
            {
                container::remove_stale(docker, name.trim()).await;
            }
        }
    }
    host::sweep(data_dir, project);
}
