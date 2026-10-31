//! A process tree that dies whole: 07.4's "Process trees die whole"
//! (docs/specs/agent/07.4-stopping-and-restarting.md). A spawned child joins one right after
//! `spawn()`, via [`ProcessTree::join`] — after [`ProcessTree::prepare`] has set the command up
//! for it, which only Linux and macOS need before the child starts. Everything the child spawns
//! while it's running joins the same tree: a Job object with
//! `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` on Windows, the child's own process group on Linux and
//! macOS. [`ProcessTree::kill_and_wait`] (or simply dropping the tree, on Windows) takes the
//! whole thing down in one go, not just the direct child.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::process::{Child, Command};

/// How long a graceful stop waits for a tree's kill before giving up on it (07.4's "A graceful
/// stop waits for the kills ... 5 seconds at most per session").
pub const KILL_LIMIT: Duration = Duration::from_secs(5);

/// A killable process tree: everything a spawned child starts, grouped so [`kill_and_wait`] (or
/// a drop, on Windows) takes it down whole.
///
/// [`kill_and_wait`]: ProcessTree::kill_and_wait
pub struct ProcessTree {
    cut: AtomicBool,
    group: platform::Group,
    pid: u32,
}

/// Every live [`ProcessTree`], so [`cut_all`] can reach them all: each tree adds itself in
/// [`ProcessTree::join`] and is pruned once it's gone, by its own `Drop` or by [`cut_all`]
/// walking the list.
fn registry() -> &'static Mutex<Vec<Weak<ProcessTree>>> {
    static REGISTRY: OnceLock<Mutex<Vec<Weak<ProcessTree>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Vec::new()))
}

/// Set by [`cut_all`]; read back by [`stopping`].
static STOPPING: AtomicBool = AtomicBool::new(false);

/// Whether a restart or Quit is cutting every session right now. The orchestrator loop checks
/// this so it neither starts nor claims new work once it's true
/// (docs/specs/agent/07.4-stopping-and-restarting.md, `restart-cuts-sessions`).
pub fn stopping() -> bool {
    STOPPING.load(Ordering::SeqCst)
}

/// Cuts every live process tree at once: sets [`stopping`], then calls `cut()` on every
/// registered tree and waits for all of them in parallel, [`KILL_LIMIT`] at most each (07.4's
/// "A graceful stop waits for the kills"). Logs the pid of any tree that stayed past the limit.
pub fn cut_all() {
    STOPPING.store(true, Ordering::SeqCst);
    let trees: Vec<Arc<ProcessTree>> = {
        let mut reg = registry().lock().unwrap();
        reg.retain(|w| w.strong_count() > 0);
        reg.iter().filter_map(Weak::upgrade).collect()
    };
    for tree in &trees {
        tree.cut();
    }
    let waits: Vec<_> = trees
        .into_iter()
        .map(|tree| std::thread::spawn(move || (tree.pid, tree.kill_and_wait(KILL_LIMIT))))
        .collect();
    for wait in waits {
        if let Ok((pid, gone)) = wait.join()
            && !gone
        {
            tracing::warn!("process tree {pid} stayed after the kill");
        }
    }
}

/// Resets the process-wide flag and registry `cut_all` uses, so a test that exercises it
/// doesn't leave `stopping()` true, or stale entries behind, for whatever runs after it.
#[cfg(test)]
pub(crate) fn reset_for_tests() {
    STOPPING.store(false, Ordering::SeqCst);
    registry().lock().unwrap().clear();
}

/// How many live trees the registry currently holds, pruning dead entries first.
#[cfg(test)]
pub(crate) fn registered_count() -> usize {
    let mut reg = registry().lock().unwrap();
    reg.retain(|w| w.strong_count() > 0);
    reg.len()
}

impl ProcessTree {
    /// Sets `command` up so the child it spawns starts its own tree. On Linux and macOS that
    /// means the process group itself (`process_group(0)`); call this before `.spawn()`. Windows
    /// needs nothing here — its tree (a Job object) is created after spawn, in [`join`].
    ///
    /// [`join`]: ProcessTree::join
    pub fn prepare(command: &mut Command) {
        #[cfg(not(windows))]
        command.process_group(0);
        #[cfg(windows)]
        let _ = command;
    }

    /// Puts `child` in a fresh tree, right after it's spawned: a Job object on Windows, assigned
    /// to it now; the process group [`prepare`] already put it in on Linux and macOS. Registers
    /// the tree so [`cut_all`] can reach it for as long as it stays alive.
    ///
    /// [`prepare`]: ProcessTree::prepare
    pub fn join(child: &Child) -> Result<Arc<Self>> {
        let pid = child.id().context("the session has already exited")?;
        let group = platform::Group::join(child)?;
        let tree = Arc::new(Self {
            cut: AtomicBool::new(false),
            group,
            pid,
        });
        registry().lock().unwrap().push(Arc::downgrade(&tree));
        Ok(tree)
    }

    /// Marks this tree as cut by the agent itself (a restart or Quit), not something that died
    /// or crashed on its own — read back by [`was_cut`]. Call before killing it.
    ///
    /// [`was_cut`]: ProcessTree::was_cut
    pub fn cut(&self) {
        self.cut.store(true, Ordering::SeqCst);
    }

    /// Whether [`cut`] has been called on this tree.
    ///
    /// [`cut`]: ProcessTree::cut
    pub fn was_cut(&self) -> bool {
        self.cut.load(Ordering::SeqCst)
    }

    /// Kills everything in the tree and waits for it to be gone, up to `limit`. Returns whether
    /// it went within that time.
    pub fn kill_and_wait(&self, limit: Duration) -> bool {
        self.group.kill_and_wait(limit)
    }
}

impl Drop for ProcessTree {
    fn drop(&mut self) {
        registry().lock().unwrap().retain(|w| w.strong_count() > 0);
    }
}

#[cfg(windows)]
mod platform {
    use std::mem::size_of;
    use std::time::{Duration, Instant};

    use anyhow::{Context, Result, bail};
    use tokio::process::Child;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_BASIC_LIMIT_INFORMATION,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
        QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
    };

    /// A Job object the child (and everything it spawns) was assigned to, with
    /// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` set so closing its last handle — on a drop — kills
    /// whatever is still in it.
    pub(super) struct Group(HANDLE);

    // A job handle is a reference to a kernel object, not something tied to the thread that
    // created it: querying, terminating or closing it from another thread (e.g. inside
    // `spawn_blocking`) is fine.
    unsafe impl Send for Group {}
    unsafe impl Sync for Group {}

    impl Group {
        pub(super) fn join(child: &Child) -> Result<Self> {
            let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if job.is_null() {
                bail!("creating the job object: {}", std::io::Error::last_os_error());
            }
            let group = Self(job);

            let info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION {
                BasicLimitInformation: JOBOBJECT_BASIC_LIMIT_INFORMATION {
                    LimitFlags: JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                    ..Default::default()
                },
                ..Default::default()
            };
            let ok = unsafe {
                SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const _,
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };
            if ok == 0 {
                bail!(
                    "setting the job object's kill-on-close limit: {}",
                    std::io::Error::last_os_error()
                );
            }

            let process = child.raw_handle().context("the session has already exited")? as HANDLE;
            let ok = unsafe { AssignProcessToJobObject(job, process) };
            if ok == 0 {
                bail!(
                    "assigning the session to its job object: {}",
                    std::io::Error::last_os_error()
                );
            }
            Ok(group)
        }

        pub(super) fn kill_and_wait(&self, limit: Duration) -> bool {
            unsafe { TerminateJobObject(self.0, 1) };
            let start = Instant::now();
            loop {
                if self.active_processes() == 0 {
                    return true;
                }
                if start.elapsed() >= limit {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }

        /// The job's active process count, via `JobObjectBasicAccountingInformation`. A failed
        /// query reports 1 (treated as "still has something in it") rather than 0, so a transient
        /// failure can't be mistaken for the tree already being gone.
        fn active_processes(&self) -> u32 {
            let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
            let ok = unsafe {
                QueryInformationJobObject(
                    self.0,
                    JobObjectBasicAccountingInformation,
                    &mut info as *mut _ as *mut _,
                    size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 { 1 } else { info.ActiveProcesses }
        }
    }

    impl Drop for Group {
        fn drop(&mut self) {
            // `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`: closing the job's last handle terminates
            // whatever is still running in it — this is how dropping the tree takes it with it.
            unsafe { CloseHandle(self.0) };
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use std::process::Command;
    use std::time::{Duration, Instant};

    use anyhow::{Context, Result};
    use tokio::process::Child;

    /// The process group id `ProcessTree::prepare`'s `process_group(0)` put the child in (its
    /// own pid, since `process_group(0)` makes it its own group leader).
    pub(super) struct Group(u32);

    impl Group {
        pub(super) fn join(child: &Child) -> Result<Self> {
            Ok(Self(child.id().context("the session has already exited")?))
        }

        pub(super) fn kill_and_wait(&self, limit: Duration) -> bool {
            let start = Instant::now();
            signal(self.0, "-TERM");
            if wait_until_gone(self.0, start, limit / 2) {
                return true;
            }
            signal(self.0, "-KILL");
            wait_until_gone(self.0, start, limit)
        }
    }

    fn signal(pgid: u32, flag: &str) {
        let _ = Command::new("kill").args([flag, "--", &format!("-{pgid}")]).output();
    }

    /// Whether anything in `pgid` is still actually running. A killed process whose parent never
    /// reaps it (nothing here waits on the grandchildren the direct child spawned, nor, while
    /// this is polling, on the direct child itself) lingers as a zombie: still in the process
    /// table with `pgid` intact, so `kill -0` on the group keeps succeeding forever even once
    /// every process in it is dead. A zombie can't run anything, so it's excluded here by
    /// checking each process's state instead of just whether the group still resolves to one.
    fn group_alive(pgid: u32) -> bool {
        let pgid = pgid.to_string();
        Command::new("ps")
            .args(["-axo", "pid=,pgid=,state="])
            .output()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout).lines().any(|line| {
                    let mut columns = line.split_whitespace();
                    let _pid = columns.next();
                    columns.next() == Some(pgid.as_str()) && columns.next().is_some_and(|state| !state.starts_with('Z'))
                })
            })
            .unwrap_or(false)
    }

    fn wait_until_gone(pgid: u32, start: Instant, budget: Duration) -> bool {
        loop {
            if !group_alive(pgid) {
                return true;
            }
            if start.elapsed() >= budget {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    use super::*;

    /// Waits for `path` to exist and be non-empty, so a test only starts timing a kill once the
    /// grandchild it's watching has actually ticked at least once.
    fn wait_for_first_tick(path: &std::path::Path) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if std::fs::metadata(path).map(|m| m.len() > 0).unwrap_or(false) {
                return;
            }
            assert!(Instant::now() < deadline, "the grandchild never ticked");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[tokio::test]
    async fn kill_and_wait_takes_the_whole_tree_including_a_grandchild_within_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let (bin, args, ticks) = crate::test_support::grandchild_tick_command(dir.path());
        let mut cmd = Command::new(bin);
        cmd.args(&args)
            .current_dir(dir.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        ProcessTree::prepare(&mut cmd);
        let child = cmd.spawn().unwrap();
        let tree = ProcessTree::join(&child).unwrap();

        wait_for_first_tick(&ticks);
        let size_before_kill = std::fs::metadata(&ticks).unwrap().len();

        let started = Instant::now();
        assert!(tree.kill_and_wait(Duration::from_secs(5)));
        assert!(started.elapsed() < Duration::from_secs(5));

        let size_right_after = std::fs::metadata(&ticks).unwrap().len();
        std::thread::sleep(Duration::from_millis(400));
        let size_later = std::fs::metadata(&ticks).unwrap().len();
        assert!(size_right_after >= size_before_kill);
        assert_eq!(
            size_right_after, size_later,
            "the grandchild kept touching its file after the kill"
        );
    }

    #[tokio::test]
    async fn was_cut_is_false_until_cut_is_called() {
        let dir = tempfile::tempdir().unwrap();
        let mut cmd = Command::new(if cfg!(windows) { "cmd" } else { "true" });
        if cfg!(windows) {
            cmd.args(["/C", "exit 0"]);
        }
        cmd.current_dir(dir.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        ProcessTree::prepare(&mut cmd);
        let mut child = cmd.spawn().unwrap();
        let tree = ProcessTree::join(&child).unwrap();
        let _ = child.wait().await;

        assert!(!tree.was_cut());
        tree.cut();
        assert!(tree.was_cut());
    }

    #[tokio::test]
    async fn cut_all_takes_every_registered_tree_including_grandchildren_and_sets_stopping() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let (bin_a, args_a, ticks_a) = crate::test_support::grandchild_tick_command(dir_a.path());
        let (bin_b, args_b, ticks_b) = crate::test_support::grandchild_tick_command(dir_b.path());

        let mut cmd_a = Command::new(bin_a);
        cmd_a
            .args(&args_a)
            .current_dir(dir_a.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        ProcessTree::prepare(&mut cmd_a);
        let child_a = cmd_a.spawn().unwrap();
        let tree_a = ProcessTree::join(&child_a).unwrap();

        let mut cmd_b = Command::new(bin_b);
        cmd_b
            .args(&args_b)
            .current_dir(dir_b.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        ProcessTree::prepare(&mut cmd_b);
        let child_b = cmd_b.spawn().unwrap();
        let tree_b = ProcessTree::join(&child_b).unwrap();

        wait_for_first_tick(&ticks_a);
        wait_for_first_tick(&ticks_b);

        cut_all();
        assert!(stopping());
        assert!(tree_a.was_cut());
        assert!(tree_b.was_cut());

        let size_a_right_after = std::fs::metadata(&ticks_a).unwrap().len();
        let size_b_right_after = std::fs::metadata(&ticks_b).unwrap().len();
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(
            size_a_right_after,
            std::fs::metadata(&ticks_a).unwrap().len(),
            "a's grandchild kept ticking after cut_all"
        );
        assert_eq!(
            size_b_right_after,
            std::fs::metadata(&ticks_b).unwrap().len(),
            "b's grandchild kept ticking after cut_all"
        );

        reset_for_tests();
    }

    #[tokio::test]
    async fn a_tree_that_ended_on_its_own_leaves_the_registry() {
        let dir = tempfile::tempdir().unwrap();
        let mut cmd = Command::new(if cfg!(windows) { "cmd" } else { "true" });
        if cfg!(windows) {
            cmd.args(["/C", "exit 0"]);
        }
        cmd.current_dir(dir.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        ProcessTree::prepare(&mut cmd);
        let mut child = cmd.spawn().unwrap();
        let tree = ProcessTree::join(&child).unwrap();
        let weak = Arc::downgrade(&tree);
        let _ = child.wait().await;

        let before = registered_count();
        assert!(weak.strong_count() > 0, "the tree is still registered while it's held");
        drop(tree);
        let after = registered_count();

        assert_eq!(weak.strong_count(), 0);
        assert_eq!(
            after,
            before - 1,
            "dropping the tree removed exactly it from the registry"
        );
    }
}
