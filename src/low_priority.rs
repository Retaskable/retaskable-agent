//! docs/specs/agent-ci.md section 3.1 (Low priority): the loop's `git` and `docker` children start at
//! below-normal priority on every platform, so CI never competes with the sessions for the processor.
use std::ffi::OsStr;
use std::process::Command;

/// Windows `BELOW_NORMAL_PRIORITY_CLASS`.
#[cfg(windows)]
const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;
/// The `nice` increment on Unix.
#[cfg(not(windows))]
const NICE: &str = "10";

/// A command for `program` that starts at below-normal priority (and, on Windows, opens no window): the creation flag on Windows, `nice -n 10`
/// elsewhere (so `get_program` is `nice` and the program is its first real argument).
pub fn low_priority(program: impl AsRef<OsStr>) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut cmd = Command::new(program);
        cmd.creation_flags(BELOW_NORMAL_PRIORITY_CLASS | crate::spawn::CREATE_NO_WINDOW);
        cmd
    }
    #[cfg(not(windows))]
    {
        let mut cmd = Command::new("nice");
        cmd.args(["-n", NICE]).arg(program);
        cmd
    }
}

/// Lowers this whole program to below-normal priority, inherited by everything it starts (agent.md section 4):
/// `nice` 10 where there is one (Linux gives a thread its own, so this must run before the runtime makes its threads),
/// `BELOW_NORMAL_PRIORITY_CLASS` for the process on Windows.
pub fn lower_self() -> Result<(), String> {
    #[cfg(unix)]
    {
        // SAFETY: `setpriority` takes three integers and touches no memory of ours.
        let r = unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, 10) };
        if r != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, SetPriorityClass};
        // SAFETY: the current process's pseudo-handle is always valid.
        if unsafe { SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS) } == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(())
    }
}

/// [`low_priority`] for tokio; `kill_on_drop` is the caller's to set.
pub fn low_priority_tokio(program: impl AsRef<OsStr>) -> tokio::process::Command {
    tokio::process::Command::from(low_priority(program))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(windows))]
    #[test]
    fn unix_wraps_the_program_in_nice() {
        let cmd = low_priority("git");
        assert_eq!(cmd.get_program(), "nice");
        let args: Vec<_> = cmd.get_args().collect();
        assert_eq!(args, ["-n", "10", "git"]);
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn unix_child_runs_with_a_raised_nice_value() {
        let mut cmd = low_priority_tokio("sh");
        cmd.args(["-c", "nice"]);
        let out = cmd.output().await.unwrap();
        let ni: i32 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap();
        assert!(ni >= 10, "nice value was {ni}");
    }

    #[cfg(windows)]
    #[test]
    fn windows_sets_the_below_normal_class_and_still_runs() {
        let cmd = low_priority("cmd");
        assert_eq!(cmd.get_program(), "cmd");
        let out = low_priority("cmd").args(["/C", "echo", "hi"]).output().unwrap();
        assert!(out.status.success());
        assert_eq!(BELOW_NORMAL_PRIORITY_CLASS, 0x4000);
    }

    /// A thread that lowered itself starts children at nice 10 (Linux gives a thread its own niceness).
    #[cfg(unix)]
    #[test]
    fn a_lowered_program_starts_its_children_at_below_normal() {
        let out = std::thread::spawn(|| {
            lower_self().unwrap();
            std::process::Command::new("nice").output().unwrap()
        })
        .join()
        .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "10");
    }
}
