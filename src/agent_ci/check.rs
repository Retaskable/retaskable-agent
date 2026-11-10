//! docs/specs/agent-ci.md sections 3.2 and 3.3: one claimed check end to end. The caller holds the slot's permit
//! for the whole of it: pending, fetch (`host.rs`), a conflict fails it with no container, else run
//! (`container.rs`), then success or failure (and on failure the comment), then the directory goes.
use crate::text::is_blank;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use serde_json::json;

use super::claim::{self, Verdict};
use super::report::{self, Host, Kind, State};
use super::{CheckKey, Programs, RunningCheck, automerge, container, host};
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
    /// Merges the pull request when the check is green; none merges nothing.
    pub automerge: Option<automerge::Automerge>,
    pub clock: super::Clock,
    pub refresh: std::time::Duration,
}

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

fn short(sha: &str) -> String {
    sha.chars().take(7).collect()
}

/// `<head>-<main>`, short: the key as the failure comment and events carry it.
pub fn key_text(k: &CheckKey) -> String {
    if k.is_push() {
        return short(&k.main);
    }
    format!("{}-{}", short(&k.head), short(&k.main))
}

/// Writes a `ci` event to the project's log.
fn emit(j: &Job, state: &str, ms: u128) {
    let mut log = EventLog::new();
    log.open(&crate::paths::project_dir(&j.data_dir, &j.id).join("events"));
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
    log.add_in(&j.id, Event::Ci(ci));
}

/// The log's last non-empty line, for the failure status.
fn last_line(tail: &str) -> &str {
    tail.lines().rev().find(|l| !is_blank(l)).unwrap_or("")
}

/// The check's directory has a sibling file naming its container, so a restart can remove a cut one.
fn name_file(dir: &Path) -> PathBuf {
    dir.with_extension("name")
}

/// Claims the key and, if this computer wins, runs the check; the last status is posted before this returns.
/// Failures to reach the host are logged: the check still counts as finished, so a broken `gh` doesn't rerun
/// the container every poll. A key that can't be claimed (host unreachable) is tried again next poll.
pub async fn run(j: Job) -> Ran {
    let host_api = Arc::new(Host {
        program: PathBuf::from(&j.programs.gh),
        kind: Kind::Github,
        repo: j.project.github_repo.clone(),
        dir: PathBuf::from(&j.project.repo_path),
    });
    let key = key_text(&j.key);
    match claim::claim(&host_api, context_of(&j.key), &j.key.head, &key, &j.computer, || {
        (j.clock)()
    })
    .await
    {
        Ok(Verdict::Run { stale }) => {
            if !stale.is_empty() {
                tracing::info!("agent CI: taking over {key} from {}", stale.join(", "));
                emit(&j, "stale_taken_over", 0);
            }
            emit(&j, "claimed", 0);
        }
        Ok(Verdict::Lose) => return Ran::Lost,
        Ok(Verdict::Finished) => return Ran::Done,
        Err(e) => {
            tracing::warn!("agent CI: claiming {key} failed: {e:#}");
            return Ran::Lost;
        }
    }
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
    let ok = go(&j, begun, host_api).await;
    let ms = begun.elapsed().as_millis();
    emit(&j, "finished", ms);
    j.live.update_ci(|c| {
        c.checks
            .retain(|r| !(r.project == j.id && r.pr == j.key.pr && r.sha == sha7));
        c.running = c.checks.len() as u32;
        c.last_result = Some(json!({ "project": j.id, "pr": j.key.pr, "sha": sha7, "ok": ok, "at": started_ms }));
    });
    Ran::Done
}

async fn go(j: &Job, begun: Instant, host_api: Arc<Host>) -> bool {
    let me = report::me(&j.computer);
    let repo = PathBuf::from(&j.project.repo_path);
    let key = key_text(&j.key);
    let head = j.key.head.clone();
    let minutes = || begun.elapsed().as_secs() / 60;
    let context = context_of(&j.key);
    let post = |state: State, text: String| {
        let (h, head, text) = (host_api.clone(), head.clone(), claim::describe(&text, &key));
        async move {
            let r = tokio::task::spawn_blocking(move || h.post_status(&head, context, state, &text, None)).await;
            match r {
                Ok(Err(e)) => tracing::warn!("agent CI: posting a status failed: {e:#}"),
                Err(e) => tracing::warn!("agent CI: posting a status failed: {e}"),
                Ok(Ok(())) => {}
            }
        }
    };
    // The claim is already posted; it is posted again every few minutes until the last status.
    let refresh = || post(State::Pending, report::running_text(&me));
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
    let result: Result<(), (String, String)> = claim::refreshing(j.refresh, refresh, async {
        let (data, id, main) = (j.data_dir.clone(), j.id.clone(), j.project.base_branch.clone());
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
                let line = format!("conflicts with {}: {}", j.project.base_branch, last_line(&reason));
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

        let name = container::container_name(&j.project.github_repo, &j.key.head);
        let _ = std::fs::write(name_file(&dir), &name);
        let log_dir = crate::paths::project_dir(&j.data_dir, &j.id).join("ci-logs");
        let outcome = container::run(&container::Check {
            docker: &j.programs.docker,
            repo: &j.project.github_repo,
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
            land(j).await;
            true
        }
        Err((reason, log)) => {
            fail(reason, log).await;
            false
        }
    }
}

/// A green check's `automerge`: the pull request is squash-merged under the merge lock, or says why not.
async fn land(j: &Job) {
    let Some(am) = &j.automerge else { return };
    let repo = PathBuf::from(&j.project.repo_path);
    let target = automerge::Target {
        gh: &j.programs.gh,
        git: &j.programs.git,
        repo_dir: &repo,
        github_repo: &j.project.github_repo,
        base: &j.project.base_branch,
        computer: &j.computer,
        key: &j.key,
    };
    match automerge::run(am, &target).await {
        automerge::Ended::Merged => tracing::info!("agent CI: merged PR #{}", j.key.pr),
        automerge::Ended::MainMoved => tracing::info!("agent CI: main moved, PR #{} isn't merged", j.key.pr),
        automerge::Ended::NotAsked => {}
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
    fn short_keeps_a_sha_shorter_than_seven_whole() {
        assert_eq!(short("abc"), "abc");
        assert_eq!(short(""), "");
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
