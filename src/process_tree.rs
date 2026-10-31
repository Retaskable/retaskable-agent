//! A process tree that dies whole: 07.4's "Process trees die whole"
//! (docs/specs/agent/07.4-stopping-and-restarting.md). A spawned child joins one right after
//! `spawn()`, via [`ProcessTree::join`] — after [`ProcessTree::prepare`] has set the command up
//! for it, which only Linux and macOS need before the child starts. Everything the child spawns
//! while it's running joins the same tree: a Job object with
//! `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` on Windows, the child's own process group on Linux and
//! macOS. [`ProcessTree::kill_and_wait`] (or simply dropping the tree, on Windows) takes the
//! whole thing down in one go, not just the direct child.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Result;
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
    /// to it now; the process group [`prepare`] already put it in on Linux and macOS.
    ///
    /// [`prepare`]: ProcessTree::prepare
    pub fn join(child: &Child) -> Result<Self> {
        Ok(Self {
            cut: AtomicBool::new(false),
            group: platform::Group::join(child)?,
        })
    }

    /// The same as [`prepare`], for a plain `std::process::Command` spawned synchronously —
    /// `proc::run_command_with_timeout`'s shell has no Tokio runtime to spawn a [`Command`] into.
    ///
    /// [`prepare`]: ProcessTree::prepare
    pub fn prepare_sync(command: &mut std::process::Command) {
        #[cfg(not(windows))]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        #[cfg(windows)]
        let _ = command;
    }

    /// The same as [`join`], for the `std::process::Child` [`prepare_sync`] set up.
    ///
    /// [`join`]: ProcessTree::join
    /// [`prepare_sync`]: ProcessTree::prepare_sync
    pub fn join_sync(child: &std::process::Child) -> Result<Self> {
        Ok(Self {
            cut: AtomicBool::new(false),
            group: platform::Group::join_sync(child)?,
        })
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
            let process = child.raw_handle().context("the session has already exited")? as HANDLE;
            Self::join_handle(process)
        }

        pub(super) fn join_sync(child: &std::process::Child) -> Result<Self> {
            use std::os::windows::io::AsRawHandle;
            Self::join_handle(child.as_raw_handle() as HANDLE)
        }

        /// Creates a Job object, sets its kill-on-close limit, and assigns `process` to it — the
        /// part [`join`] and [`join_sync`] share once each has its own child's raw handle.
        ///
        /// [`join`]: Group::join
        /// [`join_sync`]: Group::join_sync
        fn join_handle(process: HANDLE) -> Result<Self> {
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

        pub(super) fn join_sync(child: &std::process::Child) -> Result<Self> {
            Ok(Self(child.id()))
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
}
