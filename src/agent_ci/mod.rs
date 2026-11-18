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
pub mod fair;
pub mod inputs;
pub mod kept_ids;
pub mod link;
pub mod mirror;
pub mod prs;
pub mod report;
pub mod serve;
pub mod settings;
pub mod steps;
pub mod store;
pub mod timing;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Semaphore;
use tokio::task::JoinHandle;

use crate::program_status::{self, Linked, Report, StatusWriter};
pub use prs::CheckKey;
pub use settings::{Repo, Settings};

/// Milliseconds since 1970.
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// How soon the store is asked again while a repo's timing is not known: the store sets every other interval (`ciPollSeconds`),
/// so this is only the pause between tries at reading it, never a rule about CI.
pub const UNKNOWN_PAUSE: Duration = Duration::from_secs(10);

/// The status file's writer, whether the store link is held now, and the names of the settings the environment sets.
type StatusFile = (StatusWriter, Arc<dyn Fn() -> bool + Send + Sync>, Vec<String>);

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
    /// Passes of `start` that had work: the repo dealt first rotates with it.
    turn: usize,
    /// The checks holding a permit now.
    running: Arc<Mutex<HashSet<Started>>>,
    /// CPUs Docker has, read once Docker answers.
    ncpu: Option<u32>,
    /// Milliseconds since 1970; a test injects its own.
    pub clock: Clock,
    /// How often a running check's claim is refreshed, when a test sets it; otherwise the store's `ciHeartbeatMinutes`.
    pub refresh: Option<Duration>,
    /// Where a repo's mirror fetches from; a test points it at a local repo.
    pub remote: Arc<dyn Fn(&Repo) -> String + Send + Sync>,
    /// The store (the lease, the run records) and the merge of green pull requests. None: nothing runs (fail
    /// closed: the store is the claim).
    merge: Option<automerge::Automerge>,
    /// Each open pull request's branch, title and author, as of the last poll.
    metas: Arc<Mutex<HashMap<(String, u64), prs::PrMeta>>>,
    /// Heads (or their prefixes) named to `once`: checked whatever their status says.
    force: Vec<String>,
    /// The pause between `ci_finish` tries.
    pub finish_pause: Duration,
    /// Docker as the last poll found it (`ok`, `missing`, `stopped`), for the status file.
    docker_state: &'static str,
    /// The status file's writer, and whether the store link is there now.
    status: Option<StatusFile>,
    /// Why the last re-read of the settings failed (`lastError` in the status file), until one succeeds.
    settings_error: Option<String>,
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
            turn: 0,
            running: Arc::default(),
            ncpu: None,
            clock: Arc::new(crate::telemetry::now_ms),
            refresh: None,
            docker_state: "unknown",
            status: None,
            settings_error: None,
            remote: Arc::new(Repo::url),
            merge: None,
            metas: Arc::default(),
            force: Vec::new(),
            finish_pause: Duration::from_secs(5),
        }
    }

    /// Gives the loop its store: the lease every check is claimed with, the run records, and the merge of green
    /// pull requests the store asks to merge. Without it nothing runs.
    pub fn with_store(mut self, store: Arc<dyn store::CiStore>) -> Self {
        self.merge = Some(automerge::Automerge::new(store));
        self
    }

    /// Keeps `<ci folder>/status.json` (programs-status.md); `linked` says whether the store link is held now, and
    /// `env_set` names the settings that came from the environment.
    pub fn with_status(
        mut self,
        writer: StatusWriter,
        linked: Arc<dyn Fn() -> bool + Send + Sync>,
        env_set: Vec<String>,
    ) -> Self {
        self.status = Some((writer, linked, env_set));
        self
    }

    /// Reads the settings again (the file in the runner's folder, under the environment). New ones replace `cfg`, so
    /// slots, CPUs, repos and the name apply on the next poll; a file that can't be read or has a bad field keeps the
    /// old ones, said once in the log and kept as `lastError` in the status file until a read works.
    pub fn reload(&mut self, cfg: &mut Settings, read: impl FnOnce() -> anyhow::Result<Settings>) {
        match read() {
            Ok(new) => {
                self.logged.remove("settings");
                self.settings_error = None;
                if new != *cfg {
                    tracing::info!(
                        "settings changed: {} slot(s), {} cpu(s), {} repo(s)",
                        new.slots,
                        new.cpus,
                        new.repos.len()
                    );
                    *cfg = new;
                }
            }
            Err(e) => {
                let why = format!("{e:#}");
                self.say_once("settings", &format!("keeping the old settings: {why}"));
                self.settings_error = Some(why);
            }
        }
    }

    /// One write of the status file, if there is a writer.
    fn write_status(&self, cfg: &Settings) {
        let Some((writer, linked, env_set)) = &self.status else {
            return;
        };
        let mut extra = serde_json::Map::new();
        extra.insert("docker".into(), self.docker_state.into());
        extra.insert("slots".into(), cfg.slots.into());
        extra.insert("slotsBusy".into(), self.running.lock().unwrap().len().into());
        writer.write_or_log(
            program_status::now_ms(),
            Report {
                linked: Linked {
                    store: linked(),
                    cloud: None,
                },
                last_error: self.settings_error.clone(),
                env_set: env_set.clone(),
                extra,
            },
        );
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
            let state = crate::docker_probe::probe(&self.programs.docker, false).await;
            self.docker_state = state.as_str();
            match state {
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

    /// Whether a check of `key`'s head is running: against another main (a new main makes a new key) or for
    /// another pull request at the same commit. A commit's status and its containers' names are per commit and
    /// event, so they share the one run and its result (2026-11-18, #797 and #798 at one head: a name conflict).
    fn head_running(&self, id: &str, key: &CheckKey) -> bool {
        self.running
            .lock()
            .unwrap()
            .iter()
            .any(|(rid, k)| rid == id && k.head == key.head && k.is_push() == key.is_push() && k != key)
    }

    /// Starts keys not already seen that find a free permit (held until its last status), the repos taking turns
    /// (`fair`). A key with no permit waits for the next poll.
    pub async fn start(&mut self, cfg: &Settings, keys: BTreeMap<String, Vec<CheckKey>>) -> Vec<JoinHandle<()>> {
        let mut tasks = Vec::new();
        let Some(merge) = self.merge.clone() else {
            return tasks;
        };
        // Each repo's waiting keys, then dealt a turn at a time (`fair::deal`), the first repo rotating each pass.
        let mut repos = Vec::new();
        let mut queues = Vec::new();
        for (id, list) in keys {
            let Some(repo) = cfg.repos.iter().find(|r| r.id() == id) else {
                continue;
            };
            // The timing is the store's: a repo whose timing can't be read starts no check (fail closed).
            let Some(timing) = merge.timing(&repo.github_repo) else {
                if !list.is_empty() {
                    self.say_once(&id, "the store sent no CI timing, so no check starts");
                }
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
            let waiting: Vec<CheckKey> = {
                let seen = self.seen.lock().unwrap();
                (list.into_iter())
                    .filter(|k| !seen.contains(&(id.clone(), k.clone())))
                    .collect()
            };
            if !waiting.is_empty() {
                repos.push((id, repo, mirror, has_warm, timing));
                queues.push(waiting);
            }
        }
        let first = self.turn;
        if !queues.is_empty() {
            self.turn = self.turn.wrapping_add(1);
        }
        let per_turn: Vec<usize> = repos.iter().map(|r| r.4.deal_per_turn as usize).collect();
        for (at, key) in fair::deal(queues, first, |i| per_turn[i]) {
            let (id, repo, mirror, has_warm, timing) = &repos[at];
            let (id, has_warm) = (id.clone(), *has_warm);
            {
                let started = (id.clone(), key.clone());
                if self.seen.lock().unwrap().contains(&started) {
                    continue;
                }
                // One check of a head at a time: a new main makes a new key for a pull request still being checked
                // against the old one, and its steps' containers share the head's names (`steps::Plan::name`), so
                // starting it would remove the running check's containers and fail both (2026-11-15, PR #3835 twice).
                // The next poll after that check ends starts this one.
                if self.head_running(&id, &key) {
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
                    repo: (*repo).clone(),
                    mirror: mirror.clone(),
                    cpus: self.cpus(cfg),
                    slots: cfg.slots,
                    has_warm,
                    key: key.clone(),
                    merge: merge.clone(),
                    meta,
                    force: self
                        .force
                        .iter()
                        .any(|s| !s.is_empty() && key.head.starts_with(s.as_str())),
                    clock: self.clock.clone(),
                    timing: *timing,
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

    /// Sends again, at once and then every half minute, each `ci_finish` kept in the data folder from before this start,
    /// from a task of its own: the store takes a check's record from the computer whose lease lapsed, and a restart in
    /// an outage must not lose it. Nothing is read or sent without a store.
    pub fn resend_kept(&self, cfg: &Settings) {
        let Some(merge) = &self.merge else {
            return;
        };
        for (kept, record) in check::Kept::all(&cfg.data_dir) {
            tokio::spawn(check::keep_finishing(merge.store().clone(), kept, record, true));
        }
    }

    /// How long to wait for the next poll: the shortest `ciPollSeconds` of the repos whose timing is known, else
    /// [`UNKNOWN_PAUSE`] while none is.
    pub fn poll_pause(&self, cfg: &Settings) -> Duration {
        let merge = self.merge.as_ref();
        (cfg.repos.iter())
            .filter_map(|r| merge?.timing(&r.github_repo))
            .map(|t| t.poll())
            .min()
            .unwrap_or(UNKNOWN_PAUSE)
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
        // The facts the store decides the merges from, and its answer: which green pull requests to merge, and which wait.
        let (facts, agents) = (prs::facts(&list)?, prs::agent_branches(&list)?);
        let turn = merge.turn(&host, base, &agents).await;
        merge.refresh_requests(&repo.github_repo, facts, &turn).await;
        let metas = prs::metas(&list)?;
        self.metas.lock().unwrap().retain(|(i, _), _| *i != id);
        for (n, m) in &metas {
            self.metas.lock().unwrap().insert((id.clone(), *n), m.clone());
        }
        merge.tell_closed(&host).await;
        // Green pull requests the store asks to merge (the label, a person's Merge, a hold that ended): the check that made them
        // green is over, so nothing else would merge them. Only a request with `merge: true` is merged, pinned to the head checked.
        for k in &ready {
            if merge.request(k.pr, &k.head).is_none() {
                continue;
            }
            let (h, head) = (host.clone(), k.head.clone());
            let Ok(Ok(all)) = tokio::task::spawn_blocking(move || h.read_statuses(&head)).await else {
                continue;
            };
            if claim::newest(&all, report::PR_CONTEXT).is_some_and(|s| s.state == "success") {
                merge.run(&host, k.pr, &k.head).await;
            }
        }
        // The baseline: main's own push run (the key is main's sha alone, so a new sha is a new run).
        ready.push(CheckKey::push(main.trim()));
        Ok(ready)
    }
}

/// The loop, until the process stops: recovery first, then a poll and what it starts, each `ciPollSeconds` (the store's). `read` is the
/// settings read again before each poll (`CiLoop::reload`).
pub async fn run(mut ci: CiLoop, mut cfg: Settings, mut read: impl FnMut() -> anyhow::Result<Settings>) {
    ci.write_status(&cfg);
    ci.recover(&cfg).await;
    ci.resend_kept(&cfg);
    loop {
        ci.reload(&mut cfg, &mut read);
        let keys = ci.poll(&cfg).await;
        ci.start(&cfg, keys).await;
        ci.write_status(&cfg);
        tokio::time::sleep(ci.poll_pause(&cfg)).await;
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
        for (id, list) in &mut keys {
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
mod reload_tests;
#[cfg(test)]
mod tests;
