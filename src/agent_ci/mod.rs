//! docs/specs/agent-ci.md: running CI on this computer, as its own process (`retaskable-ci`, `src/bin/ci.rs`).
//! This is the loop. Each poll it checks Docker, then for each repo in `AGENT_CI_REPO` updates the repo's bare
//! mirror (`mirror.rs`), looks for the repo's CI files on its base branch and lists the pull requests to check.
//! Checks are listed here, not run. `exec.rs` runs a check's three containers (`steps.rs` their arguments and scripts); `report.rs` is the code host side:
//! statuses and the failure comment, `store.rs` the store ops (the lease and the run records). `AGENT_CI_SLOTS` permits bound the checks.
pub mod automerge;
pub mod check;
pub mod claim;
pub mod container;
pub mod discovery;
pub mod etag;
pub mod exec;
pub mod inputs;
pub mod link;
pub mod mirror;
pub mod prs;
pub mod report;
pub mod settings;
pub mod steps;
pub mod store;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Semaphore;
use tokio::task::JoinHandle;

pub use prs::CheckKey;
pub use settings::{Repo, Settings};

/// Milliseconds since 1970.
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// How often the loop polls.
pub const POLL: Duration = Duration::from_secs(10);

/// The programs the loop starts; a test points them at stubs.
#[derive(Debug, Clone)]
pub struct Programs {
    pub git: OsString,
    pub gh: OsString,
    pub docker: OsString,
}

impl Default for Programs {
    fn default() -> Self {
        Self {
            git: "git".into(),
            gh: "gh".into(),
            docker: "docker".into(),
        }
    }
}

/// A check in flight or finished: the repo's id and its key.
type Started = (String, CheckKey);

pub struct CiLoop {
    programs: Programs,
    slot: Arc<Semaphore>,
    permits: u32,
    /// Permits a shrink couldn't take back because checks held them: each is forgotten as it returns.
    owed: Arc<AtomicU32>,
    /// The reason last logged for each repo, so a standing reason is said once.
    logged: HashMap<String, String>,
    /// Checks started (running or finished) by this loop, so none runs twice.
    seen: Arc<Mutex<HashSet<Started>>>,
    /// The checks holding a permit now.
    running: Arc<Mutex<HashSet<Started>>>,
    /// CPUs Docker has, read once Docker answers.
    ncpu: Option<u32>,
    /// Milliseconds since 1970; a test injects its own.
    pub clock: Clock,
    /// How often a running check's claim is refreshed.
    pub refresh: Duration,
    /// Where a repo's mirror fetches from; a test points it at a local repo.
    pub remote: Arc<dyn Fn(&Repo) -> String + Send + Sync>,
    /// The store (the lease, the run records) and the merge of green pull requests. None: nothing runs (fail
    /// closed: the store is the claim).
    merge: Option<automerge::Automerge>,
    /// Each open pull request's branch, title, author and `automerge` label, as of the last poll.
    metas: Arc<Mutex<HashMap<(String, u64), prs::PrMeta>>>,
    /// Heads (or their prefixes) named to `once`: checked whatever their status says.
    force: Vec<String>,
    /// The pause between `ci_finish` tries.
    pub finish_pause: Duration,
}

impl CiLoop {
    pub fn new(programs: Programs) -> Self {
        Self {
            programs,
            slot: Arc::new(Semaphore::new(0)),
            permits: 0,
            owed: Arc::new(AtomicU32::new(0)),
            logged: HashMap::new(),
            seen: Arc::default(),
            running: Arc::default(),
            ncpu: None,
            clock: Arc::new(crate::telemetry::now_ms),
            refresh: claim::REFRESH,
            remote: Arc::new(Repo::url),
            merge: None,
            metas: Arc::default(),
            force: Vec::new(),
            finish_pause: Duration::from_secs(5),
        }
    }

    /// Gives the loop its store: the lease every check is claimed with, the run records, and the merge of green
    /// pull requests (labelled `automerge`, or a person's Merge). Without it nothing runs.
    pub fn with_store(mut self, store: Arc<dyn store::CiStore>) -> Self {
        self.merge = Some(automerge::Automerge::new(store));
        self
    }

    /// The slot: a check holds a permit from its claim to its last status.
    pub fn slot(&self) -> Arc<Semaphore> {
        self.slot.clone()
    }

    fn resize(&mut self, want: u32) {
        if want > self.permits {
            let mut add = want - self.permits;
            // A shrink still waiting on held permits is undone first.
            let taken = (self.owed)
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |o| Some(o - o.min(add)))
                .map_or(0, |before| before.min(add));
            add -= taken;
            self.slot.add_permits(add as usize);
        } else {
            let diff = self.permits - want;
            let forgotten = self.slot.forget_permits(diff as usize) as u32;
            self.owed.fetch_add(diff - forgotten, Ordering::SeqCst);
        }
        self.permits = want;
    }

    /// `AGENT_CI_CPUS`, capped at the CPUs Docker has.
    fn cpus(&self, cfg: &Settings) -> u32 {
        self.ncpu.map_or(cfg.cpus, |n| n.min(cfg.cpus))
    }

    /// One poll: Docker (probed again every time, so a started Docker resumes without a restart), the slot's
    /// size, and for each repo its discovery and the checks to run.
    pub async fn poll(&mut self, cfg: &Settings) -> BTreeMap<String, Vec<CheckKey>> {
        let docker = if cfg.slots == 0 {
            Err("CI slots is 0: this computer runs no CI".to_string())
        } else {
            match crate::docker_probe::probe(&self.programs.docker, false).await {
                crate::docker_probe::DockerState::Ok => Ok(()),
                state => Err(state.reason().unwrap_or("Docker is not usable").to_string()),
            }
        };
        match &docker {
            Err(why) => {
                self.say_once("docker", why);
            }
            Ok(()) => {
                self.logged.remove("docker");
                if self.ncpu.is_none() {
                    self.ncpu = crate::docker_probe::ncpu(&self.programs.docker).await;
                }
            }
        }
        let up = docker.is_ok();
        self.resize(if up { cfg.slots } else { 0 });
        let mut keys = BTreeMap::new();
        for repo in &cfg.repos {
            let id = repo.id();
            let found = if up { self.repo(cfg, repo).await } else { Ok(Vec::new()) };
            match found {
                Ok(k) => {
                    self.logged.remove(&id);
                    keys.insert(id, k);
                }
                Err(why) => {
                    self.say_once(&id, &why);
                }
            }
        }
        // Forget keys no longer listed, unless still running. Only for a repo whose list was read this poll:
        // otherwise a finished key would run again when the list comes back.
        let running = self.running.lock().unwrap().clone();
        self.seen.lock().unwrap().retain(|(id, k)| match keys.get(id) {
            Some(l) if up => l.contains(k) || running.contains(&(id.clone(), k.clone())),
            _ => true,
        });
        keys
    }

    /// Starts each key not already seen that finds a free permit (held until its last status). A key with no
    /// permit waits for the next poll.
    pub async fn start(&mut self, cfg: &Settings, keys: BTreeMap<String, Vec<CheckKey>>) -> Vec<JoinHandle<()>> {
        let mut tasks = Vec::new();
        let Some(merge) = self.merge.clone() else {
            return tasks;
        };
        for (id, list) in keys {
            let Some(repo) = cfg.repos.iter().find(|r| r.id() == id) else {
                continue;
            };
            let mirror = cfg.mirror(repo);
            if discovery::discover(&self.programs.git, &mirror, &repo.base_branch)
                .await
                .is_err()
            {
                continue;
            }
            let has_warm = discovery::has_file(&self.programs.git, &mirror, &repo.base_branch, "warm.sh").await;
            for key in list {
                let started = (id.clone(), key.clone());
                if self.seen.lock().unwrap().contains(&started) {
                    continue;
                }
                let Ok(permit) = self.slot.clone().try_acquire_owned() else {
                    return tasks;
                };
                let meta = if key.is_push() {
                    prs::PrMeta::default()
                } else {
                    self.metas
                        .lock()
                        .unwrap()
                        .get(&(id.clone(), key.pr))
                        .cloned()
                        .unwrap_or_default()
                };
                self.seen.lock().unwrap().insert(started.clone());
                self.running.lock().unwrap().insert(started.clone());
                let (seen, running, owed) = (self.seen.clone(), self.running.clone(), self.owed.clone());
                let job = check::Job {
                    programs: self.programs.clone(),
                    data_dir: cfg.data_dir.clone(),
                    computer: cfg.computer.clone(),
                    repo: repo.clone(),
                    mirror: mirror.clone(),
                    cpus: self.cpus(cfg),
                    has_warm,
                    key: key.clone(),
                    merge: merge.clone(),
                    meta,
                    force: self
                        .force
                        .iter()
                        .any(|s| !s.is_empty() && key.head.starts_with(s.as_str())),
                    clock: self.clock.clone(),
                    refresh: self.refresh,
                    finish_pause: self.finish_pause,
                };
                tasks.push(tokio::spawn(async move {
                    // A loser forgets the key, so the next poll looks again (the winner may go stale).
                    let ran = check::run(job).await;
                    running.lock().unwrap().remove(&started);
                    if ran == check::Ran::Lost {
                        seen.lock().unwrap().remove(&started);
                    }
                    // A shrink while this ran: this permit is one it owed, so it doesn't return.
                    let owing = owed.try_update(Ordering::SeqCst, Ordering::SeqCst, |o| o.checked_sub(1));
                    if owing.is_ok() {
                        permit.forget();
                    }
                }));
            }
        }
        tasks
    }

    /// Recovery (agent.md section 7.4): leftover directories and containers from a cut check.
    pub async fn recover(&self, cfg: &Settings) {
        for repo in &cfg.repos {
            check::recover(&self.programs.docker, &cfg.computer, &repo.github_repo).await;
        }
    }

    /// Says a repo's reason in the log unless it was the last thing said about it. Whether it did.
    fn say_once(&mut self, id: &str, why: &str) -> bool {
        if self.logged.get(id).is_some_and(|w| w == why) {
            return false;
        }
        tracing::info!("{id}: {why}");
        self.logged.insert(id.to_string(), why.to_string());
        true
    }

    /// One repo's checks, or why it has none: its mirror is brought up to date, the CI files are read from the
    /// base branch, and the ready pull requests listed.
    async fn repo(&self, cfg: &Settings, repo: &Repo) -> Result<Vec<CheckKey>, String> {
        let id = repo.id();
        let mirror = cfg.mirror(repo);
        let (git, base) = (&*self.programs.git, repo.base_branch.as_str());
        mirror::ensure(git, &mirror, &(self.remote)(repo)).await?;
        mirror::refresh(git, &mirror, base).await?;
        discovery::discover(git, &mirror, base).await?;
        let main = discovery::git(git, &mirror, &["rev-parse", &format!("origin/{base}")]).await?;
        let list = prs::list(&self.programs.gh, &mirror, &repo.github_repo, base).await?;
        let mut ready = prs::ready(&list, main.trim())?;
        let merge = self.merge.as_ref().ok_or("no store: nothing runs")?;
        let host = Arc::new(report::Host {
            program: self.programs.gh.clone().into(),
            kind: report::Kind::Github,
            repo: repo.github_repo.clone(),
            dir: mirror,
        });
        merge.refresh_asked(&repo.github_repo).await;
        let metas = prs::metas(&list)?;
        self.metas.lock().unwrap().retain(|(i, _), _| *i != id);
        for (n, m) in &metas {
            self.metas.lock().unwrap().insert((id.clone(), *n), m.clone());
        }
        merge.tell_closed(&host).await;
        // Green pull requests that now ask to merge (a late label, a person's Merge, a retry): the check that
        // made them green is over, so nothing else would merge them.
        for k in &ready {
            let labelled = metas.get(&k.pr).is_some_and(|m| m.automerge);
            if !labelled && !merge.is_asked(k.pr, &k.head) {
                continue;
            }
            let (h, head) = (host.clone(), k.head.clone());
            let Ok(Ok(all)) = tokio::task::spawn_blocking(move || h.read_statuses(&head)).await else {
                continue;
            };
            if claim::newest(&all, report::PR_CONTEXT).is_some_and(|s| s.state == "success") {
                merge.run(&host, base, k.pr, &k.head).await;
            }
        }
        // The baseline: main's own push run (the key is main's sha alone, so a new sha is a new run).
        ready.push(CheckKey::push(main.trim()));
        Ok(ready)
    }
}

/// The loop, until the process stops: recovery first, then a poll and what it starts, each `POLL`.
pub async fn run(mut ci: CiLoop, cfg: Settings) {
    ci.recover(&cfg).await;
    loop {
        let keys = ci.poll(&cfg).await;
        ci.start(&cfg, keys).await;
        tokio::time::sleep(POLL).await;
    }
}

/// `once <sha>...`: polls once and checks only the keys whose head starts with one of `shas` (a pull request's
/// head, or main's head for its push run), each key once, then waits for them. How many checks started.
pub async fn once(mut ci: CiLoop, cfg: Settings, shas: &[String]) -> usize {
    ci.recover(&cfg).await;
    ci.force = shas.to_vec();
    let mut keys = ci.poll(&cfg).await;
    for list in keys.values_mut() {
        list.retain(|k| shas.iter().any(|s| !s.is_empty() && k.head.starts_with(s.as_str())));
    }
    let mut tried: HashSet<Started> = HashSet::new();
    while keys.values().any(|l| !l.is_empty()) {
        let tasks = ci.start(&cfg, keys.clone()).await;
        // What `start` took is tried, even if a loser forgets it later; what found no permit is tried again.
        tried.extend(ci.seen.lock().unwrap().iter().cloned());
        for (id, list) in keys.iter_mut() {
            list.retain(|k| !tried.contains(&(id.clone(), k.clone())));
        }
        if tasks.is_empty() {
            break;
        }
        for t in tasks {
            let _ = t.await;
        }
    }
    tried.len()
}

#[cfg(test)]
mod tests;
