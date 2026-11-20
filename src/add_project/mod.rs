//! **Add project** on the settings page (agent-settings.md section 4.7): a person picks one of their GitHub repos (or
//! pastes `owner/repo`), and this clones it and adds it to `config.json` so nobody edits that file by hand. It is a
//! settings change, not a command: it runs behind the page's session, CSRF, Origin, Host and this-computer checks
//! (`settings_server::project_routes`), one at a time, and the project starts paused.
//!
//! What it does, in order: finds a checkout of the repo already on this computer or clones it (`gh repo clone`, with
//! the token already in the process environment) into the default folder; reads its default branch from GitHub (the
//! base branch), its backlog file, CI and checks from the checkout as the TypeScript agent's Link project does
//! (`linkPlanned`, `configFromScan`); marks the project to start paused (`start_paused::hold`); writes the entry into
//! `config.json` atomically; and restarts the agent, because every part of it (the orchestrator, the connectors, the
//! store sockets) is built from the project list once at start and can't take one more live.
//!
//! Where clones go (`cloneDir` in `agent/src/github.ts`): beside the most recently added project's repo, else the
//! Docker image's `/work` volume when there is one, else the home folder. A token never leaves in a status, an error
//! or a log line: every message is scrubbed first.
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use regex::Regex;
use serde_json::{Value, json};

use crate::config::{self, ProjectConfig};
use crate::env::Env;
use crate::repo::{self, CodeHost, RepoSummary};
use crate::repo_picker::RepoPicker;
use crate::store_organisation::Pick;

mod files;
#[cfg(test)]
pub(crate) mod tests;

pub use files::{backlog_file, redact};

/// Where the Docker image keeps every project's checkout (docker.md section 3).
#[cfg(not(test))]
const DOCKER_WORK: &str = "/work";
/// Tests never use the machine's own `/work` (a dev box or the Docker image may have real folders there).
#[cfg(test)]
const DOCKER_WORK: &str = "/no/such/docker/work";

/// What a request to add was refused for, before anything was cloned.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    /// 400: not a repo name.
    Bad(String),
    /// 409: another add is running, or the repo is added already.
    Conflict(String),
}

/// Where the one add is.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Status {
    #[default]
    Idle,
    Working {
        repo: String,
        step: String,
    },
    Done {
        repo: String,
        id: String,
        folder: String,
    },
    Failed {
        repo: String,
        message: String,
    },
}

impl Status {
    pub fn json(&self) -> Value {
        match self {
            Status::Idle => json!({ "state": "idle" }),
            Status::Working { repo, step } => json!({ "state": "working", "repo": repo, "step": step }),
            Status::Done { repo, id, folder } => json!({
                "state": "done", "repo": repo, "id": id, "folder": folder, "paused": true, "restarting": true
            }),
            Status::Failed { repo, message } => json!({ "state": "failed", "repo": repo, "message": message }),
        }
    }
}

type CloneFn = Box<dyn Fn(&str, &Path) -> Result<()> + Send + Sync>;
type BranchFn = Box<dyn Fn(&Path) -> Option<String> + Send + Sync>;
type SpawnFn = Box<dyn Fn(Box<dyn FnOnce() + Send>) + Send + Sync>;
type RestartFn = Box<dyn Fn() + Send + Sync>;

pub struct AddProject {
    dir: PathBuf,
    env: Env,
    picker: RepoPicker,
    clone: CloneFn,
    branch: BranchFn,
    spawn: SpawnFn,
    restart: RestartFn,
    status: Mutex<Status>,
}

impl AddProject {
    /// The real `gh`, with the agent's restart (`restart` is called once an add has finished).
    pub fn new(dir: PathBuf, env: Env, restart: impl Fn() + Send + Sync + 'static) -> Arc<Self> {
        Self::with(
            dir,
            env,
            RepoPicker::new(),
            Box::new(|repo, dest| repo::clone_repo(CodeHost::Github, repo, dest)),
            Box::new(config::checkout_default_branch),
            Box::new(|job| {
                std::thread::spawn(job);
            }),
            Box::new(restart),
        )
    }

    pub fn with(
        dir: PathBuf,
        env: Env,
        picker: RepoPicker,
        clone: CloneFn,
        branch: BranchFn,
        spawn: SpawnFn,
        restart: RestartFn,
    ) -> Arc<Self> {
        Arc::new(Self {
            dir,
            env,
            picker,
            clone,
            branch,
            spawn,
            restart,
            status: Mutex::new(Status::Idle),
        })
    }

    pub fn status(&self) -> Status {
        crate::sync::lock(&self.status).clone()
    }

    fn set(&self, status: Status) {
        *crate::sync::lock(&self.status) = status;
    }

    /// The person's GitHub owners (their login first, then their organisations).
    pub fn owners(&self, fresh: bool) -> Result<Vec<String>, String> {
        self.picker.list_owners(fresh).map_err(|e| self.words(&e))
    }

    /// `owner`'s repos, each with whether it is added already. Only an owner `owners` lists is asked about.
    pub fn repos(&self, owner: &str, fresh: bool) -> Result<Vec<(RepoSummary, bool)>, Refusal> {
        let owners = self.owners(false).map_err(Refusal::Conflict)?;
        if !owners.iter().any(|o| o.eq_ignore_ascii_case(owner)) {
            return Err(Refusal::Bad(format!(
                "{owner} isn't you or one of your GitHub organisations."
            )));
        }
        let rows = self
            .picker
            .list_repos(owner, fresh)
            .map_err(|e| Refusal::Conflict(self.words(&e)))?;
        let added = self.added_repos();
        Ok(rows
            .into_iter()
            .map(|r| {
                let is_added = added.contains(&r.name_with_owner.to_lowercase());
                (r, is_added)
            })
            .collect())
    }

    /// Each added project's id and GitHub repo, for the page's list.
    pub fn projects(&self) -> Vec<(String, String)> {
        config::load(&self.dir)
            .map(|c| {
                c.projects
                    .iter()
                    .map(|p| (p.id_or_derived(), p.github_repo.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn added_repos(&self) -> Vec<String> {
        self.projects().into_iter().map(|(_, r)| r.to_lowercase()).collect()
    }

    /// The folder a clone of `name` goes in by default.
    pub fn clone_folder(&self, name: &str) -> PathBuf {
        let projects = config::load(&self.dir).map(|c| c.projects).unwrap_or_default();
        clone_dir(&projects, &self.env, Path::new(DOCKER_WORK)).join(name)
    }

    /// Starts adding `text` (`owner/repo`, or a GitHub address). Refused at once for a bad name, a repo added already,
    /// or another add still running; otherwise the work goes on in the background and `status` says where it is.
    pub fn start(self: &Arc<Self>, text: &str) -> Result<(), Refusal> {
        self.begin(text, true, Pick::default())
    }

    /// [`start`](Self::start) into the organisation `organisation` names (the wizard's pick of a retaskable.com project): kept
    /// in the project's config as `link_project` keeps it, and the agent restarts when the add is done.
    pub fn start_in(self: &Arc<Self>, text: &str, organisation: Pick) -> Result<(), Refusal> {
        self.begin(text, true, organisation)
    }

    /// [`start`](Self::start) for `link_project` from retaskable.com: the agent isn't restarted when the add is done. The
    /// caller answers its command first and then calls [`restart_now`](Self::restart_now), so the answer isn't lost.
    /// `organisation` is the one the site named: kept in the project's config (its own cloud's id and its slug), so its hello says it.
    pub fn start_holding_restart(self: &Arc<Self>, text: &str, organisation: Pick) -> Result<(), Refusal> {
        self.begin(text, false, organisation)
    }

    /// Restarts the agent, as a finished add does, to take the project that was added.
    pub fn restart_now(&self) {
        (self.restart)();
    }

    fn begin(self: &Arc<Self>, text: &str, restart_when_done: bool, organisation: Pick) -> Result<(), Refusal> {
        let repo = parse_repo(text).ok_or_else(|| {
            Refusal::Bad("Type the repo as owner/name, for example Retaskable/retaskable-agent.".to_string())
        })?;
        if self.added_repos().contains(&repo.to_lowercase()) {
            return Err(Refusal::Conflict(format!("{repo} is added already.")));
        }
        {
            let mut status = crate::sync::lock(&self.status);
            if let Status::Working { repo: busy, .. } = &*status {
                return Err(Refusal::Conflict(format!(
                    "Still adding {busy}: wait for it to finish before adding another."
                )));
            }
            *status = Status::Working {
                repo: repo.clone(),
                step: "Starting…".into(),
            };
        }
        let this = self.clone();
        (self.spawn)(Box::new(move || match this.add(&repo, organisation) {
            Ok((id, folder)) => {
                tracing::info!(
                    "Added project {id} ({repo}) at {}: paused until resumed.",
                    folder.display()
                );
                this.set(Status::Done {
                    repo,
                    id,
                    folder: folder.to_string_lossy().into_owned(),
                });
                if restart_when_done {
                    (this.restart)();
                }
            }
            Err(e) => {
                let message = this.words(&e);
                tracing::warn!("Couldn't add {repo}: {message}");
                this.set(Status::Failed { repo, message });
            }
        }));
        Ok(())
    }

    fn step(&self, repo: &str, step: &str) {
        self.set(Status::Working {
            repo: repo.to_string(),
            step: step.to_string(),
        });
    }

    /// An error as a sentence with no token in it.
    fn words(&self, e: &anyhow::Error) -> String {
        redact(&format!("{e:#}"), &self.env)
    }

    /// The whole add, blocking: returns the new project's id and checkout.
    fn add(&self, repo: &str, organisation: Pick) -> Result<(String, PathBuf)> {
        let cfg = config::load(&self.dir).context("Couldn't read config.json")?;
        files::refuse_old_format(&self.dir, &cfg)?;
        let name = repo.split('/').nth(1).unwrap_or(repo);
        let parent = clone_dir(&cfg.projects, &self.env, Path::new(DOCKER_WORK));
        self.step(repo, "Looking for a copy on this computer…");
        let (folder, cloned) = match files::find_checkout(&parent, repo) {
            Some(found) => (found, false),
            None => {
                let dest = parent.join(name);
                files::folder_problem(&dest, &self.dir, &cfg.projects)?;
                self.step(
                    repo,
                    &format!("Cloning {repo}… this can take a few minutes for a big repo."),
                );
                std::fs::create_dir_all(&parent).with_context(|| format!("Couldn't make {}", parent.display()))?;
                let made = !dest.exists();
                if let Err(e) = (self.clone)(repo, &dest) {
                    if made && dest.exists() {
                        let _ = std::fs::remove_dir_all(&dest);
                    }
                    return Err(e);
                }
                (dest, made)
            }
        };
        let finish = || self.finish(repo, &cfg.projects, &folder, organisation);
        let result = finish();
        if result.is_err() && cloned {
            let _ = std::fs::remove_dir_all(&folder);
        }
        result.map(|id| (id, folder.clone()))
    }

    /// Reads the checkout and writes the entry.
    fn finish(&self, repo: &str, existing: &[ProjectConfig], folder: &Path, organisation: Pick) -> Result<String> {
        self.step(repo, "Setting it up…");
        let base_branch = (self.branch)(folder).with_context(|| {
            format!("Couldn't tell which branch is {repo}'s main one: is it empty, or is gh signed in?")
        })?;
        let backlog = files::backlog_file(folder).with_context(|| {
            format!("{repo} has no Markdown file to keep its list of work in: add one, then try again.")
        })?;
        // GitHub's own spelling, from the checkout's origin (a clone's origin is how GitHub writes the name).
        let canonical = repo::origin_url(folder)
            .and_then(|url| repo::remote_repo(&url))
            .unwrap_or_else(|| repo.to_string());
        let id = files::unique_id(folder, existing);
        let detected = crate::scan::detect_checks(folder);
        let mut entry = json!({
            "id": id,
            "repoPath": folder.to_string_lossy(),
            "githubRepo": canonical,
            "backlogPath": backlog,
            "baseBranch": base_branch,
            "worktreesDir": files::default_worktrees_dir(folder).to_string_lossy(),
            "projectType": files::type_name(crate::scan::detect_project_type(folder)),
            "requireCi": crate::scan::has_workflows(folder),
            "checks": detected.checks,
            "playCommand": detected.play_command,
            // A new project starts Production only (release-flags.md section 3.9).
            "deploy": { "mergesTo": "production" },
        });
        if let Some(id) = organisation.id {
            entry["organisationId"] = json!(id);
        }
        if let Some(slug) = organisation.slug {
            entry["organisationSlug"] = json!(slug);
        }
        // Marked before it exists, so a crash between the two can't leave a project that starts unpaused.
        crate::start_paused::hold(&self.dir, &id).context("Couldn't mark the project to start paused")?;
        if let Err(e) = config::save_machine_settings(&self.dir, |obj| {
            let projects = obj.entry("projects").or_insert_with(|| json!([]));
            if let Value::Array(list) = projects {
                list.push(entry);
            }
        }) {
            crate::start_paused::release(&self.dir, &id);
            bail!("Couldn't save config.json: {e:#}");
        }
        Ok(id)
    }
}

/// `owner/name` from what a person typed: that, or a GitHub address of it.
pub fn parse_repo(text: &str) -> Option<String> {
    let text = text.trim();
    let from_url = if text.contains("github.com") {
        repo::remote_repo(text)
    } else {
        None
    };
    let text = from_url.as_deref().unwrap_or(text);
    static NAME: std::sync::LazyLock<Regex> =
        std::sync::LazyLock::new(|| Regex::new(r"^[A-Za-z0-9._-]+/[A-Za-z0-9._-]+$").unwrap());
    let ok = NAME.is_match(text) && text.split('/').all(|p| p != "." && p != "..");
    ok.then(|| text.to_string())
}

/// The folder clones go in by default (`cloneDir` in `agent/src/github.ts`): beside the most recently added project's
/// repo, else `docker_work` when it is a folder (the image's work volume), else the home folder.
pub fn clone_dir(projects: &[ProjectConfig], env: &Env, docker_work: &Path) -> PathBuf {
    if let Some(parent) = projects
        .iter()
        .rev()
        .filter(|p| !p.repo_path.is_empty())
        .find_map(|p| Path::new(&p.repo_path).parent().map(Path::to_path_buf))
    {
        return parent;
    }
    if docker_work.is_dir() {
        return docker_work.to_path_buf();
    }
    PathBuf::from(env.get("HOME").or_else(|| env.get("USERPROFILE")).unwrap_or_default())
}
