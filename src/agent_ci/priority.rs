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

/// A command for `program` that starts at below-normal priority: the creation flag on Windows, `nice -n 10`
/// elsewhere (so `get_program` is `nice` and the program is its first real argument).
pub fn low_priority(program: impl AsRef<OsStr>) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut cmd = Command::new(program);
        cmd.creation_flags(BELOW_NORMAL_PRIORITY_CLASS);
        cmd
    }
    #[cfg(not(windows))]
    {
        let mut cmd = Command::new("nice");
        cmd.args(["-n", NICE]).arg(program);
        cmd
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
        cmd.args(["-c", "ps -o ni= -p $$"]);
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
}
