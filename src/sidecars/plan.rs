//! What each program needs to be wanted (docs/specs/programs.md "The code agent starts them"), and the names of its
//! file, folder and log. The Chat Agent is always wanted: it is linked to the store or does nothing and says so. The CI
//! runner is wanted when `ciSlots` isn't 0, Docker answers (never inside the agent's own Docker image, which has none)
//! and a linked project's base branch has `.agent-ci/`, with the GitHub repos of those projects as `AGENT_CI_REPO`.
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use super::{Plan, Spec, Want, folders};
use crate::config;
use crate::docker_probe::{self, DockerState};
use crate::env::Env;

pub fn chat_spec(data_dir: &Path) -> Spec {
    let (folder, _) = folders(data_dir);
    Spec {
        label: "Chat Agent",
        exe: "retaskable-chat-agent",
        bin_var: "RETASKABLE_CHAT_AGENT_BIN",
        off_var: "RETASKABLE_NO_CHAT_AGENT",
        log: folder.join("agent.log"),
        folder,
    }
}

pub fn ci_spec(data_dir: &Path) -> Spec {
    let (_, folder) = folders(data_dir);
    Spec {
        label: "CI runner",
        exe: "retaskable-ci",
        bin_var: "RETASKABLE_CI_BIN",
        off_var: "RETASKABLE_NO_CI_RUNNER",
        log: folder.join("runner.log"),
        folder,
    }
}

pub fn chat_plan() -> Plan {
    Arc::new(|| Box::pin(async { Some(Want::default()) }))
}

/// A project of this computer the CI runner may check.
struct Project {
    repo: std::path::PathBuf,
    github_repo: String,
    base: String,
}

/// The slots, and the GitHub projects, from the config file as it is now.
fn read_config(dir: &Path, env: &Env) -> anyhow::Result<(u32, Vec<Project>)> {
    let mut cfg = config::load(dir)?;
    config::resolve_base_branches(&mut cfg, config::checkout_default_branch);
    let projects = cfg
        .projects
        .iter()
        .filter(|p| !p.github_repo.trim().is_empty())
        .map(|p| Project {
            repo: p.repo_path.clone().into(),
            github_repo: p.github_repo.trim().to_string(),
            base: if p.base_branch.trim().is_empty() {
                "main".into()
            } else {
                p.base_branch.trim().to_string()
            },
        })
        .collect();
    Ok((config::ci_slots(&cfg, env), projects))
}

/// Whether `origin/<base>` has `.agent-ci/`: the same read the CI runner's own discovery makes.
fn has_ci_dir(p: &Project) -> bool {
    let mut cmd = crate::git_bin::command(&p.repo);
    cmd.args([
        "rev-parse",
        "--verify",
        "--quiet",
        &format!("origin/{}:.agent-ci", p.base),
    ])
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null());
    cmd.status().is_ok_and(|s| s.success())
}

/// `owner/repo`, or `owner/repo@branch` for a base other than main (what `AGENT_CI_REPO` reads).
fn repo_arg(p: &Project) -> String {
    if p.base == "main" {
        p.github_repo.clone()
    } else {
        format!("{}@{}", p.github_repo, p.base)
    }
}

/// What Docker says.
pub type DockerProbe = Arc<dyn Fn() -> Pin<Box<dyn Future<Output = DockerState> + Send>> + Send + Sync>;

/// `docker version` from PATH; inside the agent's own Docker image always missing (no Docker there).
pub fn real_docker() -> DockerProbe {
    Arc::new(|| Box::pin(docker_probe::probe("docker", docker_probe::in_agent_image())))
}

pub fn ci_plan(env: Env, data_dir: std::path::PathBuf, docker: DockerProbe) -> Plan {
    let said: Arc<Mutex<Option<String>>> = Arc::default();
    Arc::new(move || {
        let (env, dir, said, docker) = (env.clone(), data_dir.clone(), said.clone(), docker.clone());
        Box::pin(async move {
            let note = |line: Option<String>| {
                let mut last = said.lock().unwrap();
                if *last != line {
                    if let Some(line) = &line {
                        tracing::info!("CI runner: {line}");
                    }
                    *last = line;
                }
            };
            let read = {
                let (dir, env) = (dir.clone(), env.clone());
                tokio::task::spawn_blocking(move || read_config(&dir, &env)).await
            };
            let (slots, projects) = match read {
                Ok(Ok(read)) => read,
                Ok(Err(e)) => {
                    note(Some(format!("not started, config.json can't be read ({e:#}).")));
                    return None;
                }
                Err(_) => return None,
            };
            if slots == 0 {
                note(Some("not started, ciSlots is 0.".into()));
                return None;
            }
            if projects.is_empty() {
                note(Some("not started, no project has a GitHub repo.".into()));
                return None;
            }
            match docker().await {
                DockerState::Ok => {}
                other => {
                    note(Some(format!(
                        "not started: {}.",
                        other.reason().unwrap_or("Docker doesn't answer")
                    )));
                    return None;
                }
            }
            let repos = tokio::task::spawn_blocking(move || {
                let mut repos: Vec<String> = projects.iter().filter(|p| has_ci_dir(p)).map(repo_arg).collect();
                repos.sort();
                repos.dedup();
                repos
            })
            .await
            .unwrap_or_default();
            if repos.is_empty() {
                note(Some("not started, no project's base branch has .agent-ci/.".into()));
                return None;
            }
            note(None);
            Some(Want {
                env: vec![
                    ("AGENT_CI_REPO".into(), repos.join(",")),
                    ("AGENT_CI_SLOTS".into(), slots.to_string()),
                    ("AGENT_CI_NAME".into(), crate::link::machine_name(&env)),
                ],
            })
        })
    })
}
