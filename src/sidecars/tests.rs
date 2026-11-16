//! The supervisor against fake binaries (shell scripts) in temp folders.
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use super::plan::{DockerProbe, chat_spec, ci_plan, ci_spec};
use super::*;
use crate::docker_probe::DockerState;

fn fast() -> Timing {
    Timing {
        tick: Duration::from_millis(20),
        replan: Duration::from_secs(60),
        first: Duration::from_millis(40),
        cap: Duration::from_millis(80),
        stable: Duration::from_secs(60),
    }
}

fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn wanted(env: &[(&str, &str)]) -> Plan {
    let want = Want {
        env: env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
    };
    Arc::new(move || {
        let want = want.clone();
        Box::pin(async move { Some(want) })
    })
}

fn never() -> Plan {
    Arc::new(|| Box::pin(async { None }))
}

async fn until(what: &str, mut ok: impl FnMut() -> bool) {
    for _ in 0..300 {
        if ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("never: {what}");
}

fn count(file: &Path) -> usize {
    std::fs::read_to_string(file).map_or(0, |t| t.lines().count())
}

struct Fixture {
    root: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self {
            root: tempfile::tempdir().unwrap(),
        }
    }
    fn data(&self) -> PathBuf {
        self.root.path().join("data")
    }
    fn exe_dir(&self) -> PathBuf {
        self.root.path().join("exe")
    }
    fn out(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }
    fn start(&self, spec: Spec, env: &Env, plan: Plan) -> Sidecar {
        Sidecar::start(spec, env, self.data(), Some(self.exe_dir()), plan, fast())
    }
}

#[tokio::test]
async fn it_is_started_when_installed_with_the_data_folder_and_the_planned_environment() {
    let f = Fixture::new();
    let out = f.out("seen");
    script(
        &f.exe_dir(),
        "retaskable-ci",
        &format!(
            "echo \"$1|$RETASKABLE_DATA_DIR|$AGENT_CI_REPO\" > {0}; echo hello from the runner; exec sleep 300",
            out.display()
        ),
    );
    let sc = f.start(ci_spec(&f.data()), &Env::empty(), wanted(&[("AGENT_CI_REPO", "o/r")]));
    until("it started", || out.exists() && count(&out) > 0).await;
    sc.stop().await;
    let seen = std::fs::read_to_string(&out).unwrap();
    assert_eq!(seen.trim(), format!("run|{}|o/r", f.data().display()));
    // Its output goes to its own log.
    let log = std::fs::read_to_string(f.data().join("ci").join("runner.log")).unwrap();
    assert!(log.contains("hello from the runner"), "{log}");
}

#[tokio::test]
async fn the_chat_agent_gets_its_own_log_and_the_override_variable_names_the_binary() {
    let f = Fixture::new();
    let out = f.out("seen");
    let bin = script(
        &f.root.path().join("elsewhere"),
        "chat-by-another-name",
        &format!("echo started >> {}; exec sleep 300", out.display()),
    );
    let env = Env::from_pairs([("RETASKABLE_CHAT_AGENT_BIN", bin.display().to_string())]);
    let sc = f.start(chat_spec(&f.data()), &env, wanted(&[]));
    until("it started", || count(&out) == 1).await;
    sc.stop().await;
    assert!(f.data().join("chat").join("agent.log").exists());
}

#[tokio::test]
async fn it_is_not_started_when_not_installed_off_or_not_wanted() {
    let f = Fixture::new();
    let out = f.out("seen");
    // Not installed: no file anywhere.
    let sc = f.start(ci_spec(&f.data()), &Env::empty(), wanted(&[]));
    tokio::time::sleep(Duration::from_millis(150)).await;
    sc.stop().await;
    assert!(!f.data().join("ci").join("runner.log").exists());
    // The data folder's bin/ counts as installed.
    assert!(find_binary(&ci_spec(&f.data()), &Env::empty(), None, &f.data()).is_none());
    script(
        &f.data().join("bin"),
        "retaskable-ci",
        &format!("echo started >> {}; exec sleep 300", out.display()),
    );
    assert!(find_binary(&ci_spec(&f.data()), &Env::empty(), None, &f.data()).is_some());
    script(
        &f.exe_dir(),
        "retaskable-ci",
        &format!("echo started >> {}; exec sleep 300", out.display()),
    );
    // Off with the variable (any value).
    let off = Env::from_pairs([("RETASKABLE_NO_CI_RUNNER", "1")]);
    let sc = f.start(ci_spec(&f.data()), &off, wanted(&[]));
    tokio::time::sleep(Duration::from_millis(150)).await;
    sc.stop().await;
    // Not wanted (no Docker, no slots, no repos).
    let sc = f.start(ci_spec(&f.data()), &Env::empty(), never());
    tokio::time::sleep(Duration::from_millis(150)).await;
    sc.stop().await;
    assert_eq!(count(&out), 0);
}

#[tokio::test]
async fn it_is_not_started_while_another_instance_holds_its_status_and_is_when_that_goes_stale() {
    let f = Fixture::new();
    let out = f.out("seen");
    script(
        &f.exe_dir(),
        "retaskable-ci",
        &format!("echo started >> {}; exec sleep 300", out.display()),
    );
    let spec = ci_spec(&f.data());
    let writer = |now: u64| {
        let w = program_status::StatusWriter::new(&spec.folder, program_status::CI_RUNNER, "1", now);
        let report = program_status::Report {
            linked: program_status::Linked {
                store: true,
                cloud: None,
            },
            ..Default::default()
        };
        w.write(now, report).unwrap();
    };
    // The Docker CI container on the same data folder: its status is fresh and isn't a child of this agent.
    writer(program_status::now_ms());
    let sc = f.start(spec.clone(), &Env::empty(), wanted(&[]));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(count(&out), 0, "a running instance is left alone");
    // Its file goes stale (it was stopped): now it is started.
    writer(program_status::now_ms() - 120_000);
    until("it started after the other went stale", || count(&out) == 1).await;
    sc.stop().await;
}

#[tokio::test]
async fn it_is_started_again_after_it_exits_with_a_growing_wait() {
    let f = Fixture::new();
    let out = f.out("seen");
    script(
        &f.exe_dir(),
        "retaskable-chat-agent",
        &format!("date +%s%N >> {}; exit 3", out.display()),
    );
    let sc = f.start(chat_spec(&f.data()), &Env::empty(), wanted(&[]));
    until("it was started four times", || count(&out) >= 4).await;
    sc.stop().await;
    let after = count(&out);
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(count(&out), after, "no more starts after stop");
}

#[test]
fn the_wait_doubles_to_the_cap_and_starts_over_after_a_stable_run() {
    let t = Timing::default();
    let s = Duration::from_secs;
    assert_eq!(next_wait(&t, s(0), s(1)), s(2));
    assert_eq!(next_wait(&t, s(2), s(1)), s(4));
    assert_eq!(next_wait(&t, s(40), s(1)), s(60));
    assert_eq!(next_wait(&t, s(60), s(1)), s(60));
    assert_eq!(next_wait(&t, s(60), s(61)), s(2));
}

#[tokio::test]
async fn it_is_stopped_with_the_agent_and_not_started_again() {
    let f = Fixture::new();
    let pid = f.out("pid");
    let out = f.out("seen");
    script(
        &f.exe_dir(),
        "retaskable-ci",
        &format!(
            "echo $$ > {}; echo started >> {}; exec sleep 300",
            pid.display(),
            out.display()
        ),
    );
    let sc = f.start(ci_spec(&f.data()), &Env::empty(), wanted(&[]));
    until("it started", || count(&pid) == 1).await;
    let pid: i32 = std::fs::read_to_string(&pid).unwrap().trim().parse().unwrap();
    assert!(crate::proc_table::signal(pid, 0).is_ok(), "running");
    sc.stop().await;
    assert!(crate::proc_table::signal(pid, 0).is_err(), "gone with the agent");
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(count(&out), 1);
}

#[tokio::test]
async fn a_changed_environment_starts_it_again_and_losing_the_want_stops_it() {
    let f = Fixture::new();
    let out = f.out("seen");
    script(
        &f.exe_dir(),
        "retaskable-ci",
        &format!("echo \"$AGENT_CI_REPO\" >> {}; exec sleep 300", out.display()),
    );
    let stage = Arc::new(AtomicUsize::new(0));
    let plan: Plan = {
        let stage = stage.clone();
        Arc::new(move || {
            let n = stage.load(Ordering::SeqCst);
            Box::pin(async move {
                match n {
                    0 => Some(Want {
                        env: vec![("AGENT_CI_REPO".into(), "o/a".into())],
                    }),
                    1 => Some(Want {
                        env: vec![("AGENT_CI_REPO".into(), "o/a,o/b".into())],
                    }),
                    _ => None,
                }
            })
        })
    };
    let sc = f.start(ci_spec(&f.data()), &Env::empty(), plan);
    until("first start", || count(&out) == 1).await;
    stage.store(1, Ordering::SeqCst);
    sc.replan();
    until("started again with the new repos", || count(&out) == 2).await;
    assert_eq!(std::fs::read_to_string(&out).unwrap().lines().last(), Some("o/a,o/b"));
    stage.store(2, Ordering::SeqCst);
    sc.replan();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(count(&out), 2, "not started again once it isn't wanted");
    sc.stop().await;
}

// The CI plan.

fn docker(state: DockerState) -> DockerProbe {
    Arc::new(move || {
        let state = state.clone();
        Box::pin(async move { state })
    })
}

fn git(dir: &Path, args: &[&str]) {
    let ok = std::process::Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "git {args:?}");
}

/// A checkout whose `origin/<base>` has (or hasn't) `.agent-ci/`.
fn checkout(root: &Path, name: &str, base: &str, with_ci: bool) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q"]);
    if with_ci {
        std::fs::create_dir_all(dir.join(".agent-ci")).unwrap();
        std::fs::write(dir.join(".agent-ci").join("image"), "x").unwrap();
    }
    std::fs::write(dir.join("f"), "x").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-qm", "c"]);
    git(&dir, &["update-ref", &format!("refs/remotes/origin/{base}"), "HEAD"]);
    dir
}

fn config(data: &Path, ci_slots: Option<u32>, projects: &[(&Path, &str, &str)]) {
    std::fs::create_dir_all(data).unwrap();
    let projects: Vec<_> = projects
        .iter()
        .map(|(p, repo, base)| serde_json::json!({"id": repo, "repoPath": p, "githubRepo": repo, "baseBranch": base, "backlogPath": "b"}))
        .collect();
    let mut cfg = serde_json::json!({"version": 2, "projects": projects});
    if let Some(n) = ci_slots {
        cfg["ciSlots"] = n.into();
    }
    std::fs::write(data.join("config.json"), cfg.to_string()).unwrap();
}

async fn planned(f: &Fixture, env: Env, docker: DockerProbe) -> Option<Want> {
    ci_plan(env, f.data(), docker)().await
}

#[tokio::test]
async fn the_ci_runner_is_wanted_with_the_repos_whose_base_has_agent_ci_the_slots_and_the_name() {
    let f = Fixture::new();
    let a = checkout(f.root.path(), "a", "main", true);
    let b = checkout(f.root.path(), "b", "dev", true);
    let c = checkout(f.root.path(), "c", "main", false);
    config(
        &f.data(),
        Some(3),
        &[(&a, "o/zed", "main"), (&b, "o/alpha", "dev"), (&c, "o/none", "main")],
    );
    let env = Env::from_pairs([("RETASKABLE_MACHINE", "box")]);
    let want = planned(&f, env, docker(DockerState::Ok)).await.expect("wanted");
    let get = |k: &str| want.env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    assert_eq!(get("AGENT_CI_REPO").as_deref(), Some("o/alpha@dev,o/zed"));
    assert_eq!(get("AGENT_CI_SLOTS").as_deref(), Some("3"));
    assert_eq!(get("AGENT_CI_NAME").as_deref(), Some("box"));
}

#[tokio::test]
async fn the_ci_runner_is_not_wanted_without_docker_with_no_slots_or_with_no_agent_ci() {
    let f = Fixture::new();
    let a = checkout(f.root.path(), "a", "main", true);
    config(&f.data(), Some(2), &[(&a, "o/r", "main")]);
    for state in [DockerState::Missing("none".into()), DockerState::Stopped("off".into())] {
        assert_eq!(planned(&f, Env::empty(), docker(state)).await, None, "no Docker");
    }
    assert!(planned(&f, Env::empty(), docker(DockerState::Ok)).await.is_some());
    config(&f.data(), Some(0), &[(&a, "o/r", "main")]);
    assert_eq!(
        planned(&f, Env::empty(), docker(DockerState::Ok)).await,
        None,
        "ciSlots 0"
    );
    let bare = checkout(f.root.path(), "bare", "main", false);
    config(&f.data(), Some(2), &[(&bare, "o/r", "main")]);
    assert_eq!(
        planned(&f, Env::empty(), docker(DockerState::Ok)).await,
        None,
        "no .agent-ci"
    );
    config(&f.data(), Some(2), &[]);
    assert_eq!(
        planned(&f, Env::empty(), docker(DockerState::Ok)).await,
        None,
        "no project"
    );
}
