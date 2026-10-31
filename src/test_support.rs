//! A tiny local git repo with an `origin` remote, shared by this crate's own `#[cfg(test)]`
//! modules that need real git plumbing to exercise (repo checks, galleries). Also a baseline
//! `ProjectConfig` for the item-lifecycle state machine and its callers (`item_lifecycle`,
//! `orchestrator`), since the derived `Default` leaves every round/slice cap at 0, which would
//! escalate to `needs_human` on the very first failure or claim.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::config::ProjectConfig;
use crate::triage::{KnownBug, TriageContext};

pub(crate) fn item_lifecycle_cfg() -> ProjectConfig {
    ProjectConfig {
        max_check_rounds: 3,
        max_review_rounds: 3,
        max_conflict_rounds: 5,
        max_slices_per_item: 10,
        ..Default::default()
    }
}

/// A `TriageContext` fixture shared by the triager's and the second look's tests (`timeline` and
/// `bug_title` vary between callers; everything else they need identical).
pub(crate) fn triage_context(timeline: Vec<String>, bug_title: &str) -> TriageContext {
    TriageContext {
        reason: "Review escalated: unclear which library to add.".to_string(),
        failed_in: "reviewing".to_string(),
        timeline,
        last_check_result: Some("passed".to_string()),
        pr_url: Some("https://github.com/x/y/pull/9".to_string()),
        branch: Some("retaskable/9-thing".to_string()),
        has_pushed_branch: true,
        has_session_to_continue: true,
        automatic_decisions: 1,
        transcripts_dir: Some(".retaskable/projects/x/runs/9".to_string()),
        known_bugs: vec![KnownBug {
            id: "retaskable-bug-abc123".to_string(),
            title: bug_title.to_string(),
        }],
    }
}

/// Writes `contents` as an executable file a test can spawn at once.
///
/// On Linux a program can't be started while any process still holds it open for writing
/// (`ETXTBSY`, "Text file busy"). Another test thread forking at the wrong moment hands the child
/// our write descriptor until that child execs, so a spawn right after the write sometimes fails.
/// Once a spawn has succeeded nothing has the file open for writing and nothing will again, so
/// this runs the file once, retrying while it's busy, and tests can then spawn it freely. The fake
/// programs tests write are all harmless to run with no arguments.
pub(crate) fn write_executable(path: &Path, contents: &str) {
    std::fs::write(path, contents).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        use std::process::Stdio;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let started = std::time::Instant::now();
        loop {
            match Command::new(path)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(mut child) => {
                    let _ = child.wait();
                    return;
                }
                Err(e) if e.raw_os_error() == Some(26) && started.elapsed() < std::time::Duration::from_secs(10) => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(e) => panic!("starting {}: {e}", path.display()),
            }
        }
    }
}

/// Starts a long-lived process whose command line contains `dir`, so a scan for processes inside
/// it (`lanes::processes_in`) finds it. Shared by `lanes.rs`'s and `shutdown.rs`'s own tests.
pub(crate) fn hold_process(dir: &Path) -> std::process::Child {
    #[cfg(windows)]
    {
        let script = dir.join("hold.cmd");
        // Absolute path in the script body so CIM's CommandLine matches even when the
        // launcher rewrites argv.
        let marker = dir.to_string_lossy().replace('"', "");
        std::fs::write(
            &script,
            format!("@echo off\nrem retaskable-lane {marker}\n:loop\nping -n 60 127.0.0.1 >nul\ngoto loop\n"),
        )
        .unwrap();
        Command::new("cmd")
            .args(["/C", "call", script.to_str().unwrap()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }
    #[cfg(not(windows))]
    {
        let script = dir.join("hold.sh");
        std::fs::write(&script, "#!/bin/sh\nwhile true; do sleep 3600; done\n").unwrap();
        let _ = Command::new("chmod").args(["+x", script.to_str().unwrap()]).status();
        Command::new("sh")
            .arg(&script)
            .current_dir(dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }
}

/// Starts a long-lived process whose command line contains `dir`, the same way [`hold_process`]
/// does, but detached from the test binary's own process tree: on Windows it's created through
/// WMI (`Win32_Process::Create`) rather than as this process's child, and on Unix the shell that
/// starts it backgrounds it and exits right away, which the OS then reparents to init. Either way
/// it isn't a descendant of the test process, so it stands in for a person's own process that
/// happens to sit in the same folder the agent scans — one `lanes::descendants_of` must not
/// reach. Returns its pid; the caller stops it with [`kill_unrelated_process`].
pub(crate) fn hold_unrelated_process(dir: &Path) -> u32 {
    #[cfg(windows)]
    {
        // `processes_in` matches on the directory plus a trailing separator, to tell `lane-1`
        // apart from `lane-10`; a bare directory with nothing after it never has one, so the
        // marker names a file under it instead, the same way the real command line a script's own
        // path produces always does. `rem` eats the rest of its line, so this goes after the ping
        // it would otherwise swallow; CIM still reports the whole command line verbatim either way.
        let marker = dir.join("marker.txt").to_string_lossy().replace('"', "");
        let command_line = format!("cmd.exe /c \"ping -n 120 127.0.0.1 >nul & rem retaskable-lane {marker}\"");
        // The command line is embedded in a single-quoted PowerShell string, so its own single
        // quotes (there are none in a Windows path, but keep this safe) must be doubled.
        let script = format!(
            "(Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{{CommandLine='{}'}}).ProcessId",
            command_line.replace('\'', "''")
        );
        let output = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .unwrap_or_else(|_| {
                panic!(
                    "starting an unrelated process: {}",
                    String::from_utf8_lossy(&output.stderr)
                )
            })
    }
    #[cfg(not(windows))]
    {
        let script = dir.join("hold_unrelated.sh");
        let pidfile = dir.join("hold_unrelated.pid");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\necho $$ > '{}'\nwhile true; do sleep 3600; done\n",
                pidfile.display()
            ),
        )
        .unwrap();
        let _ = Command::new("chmod").args(["+x", script.to_str().unwrap()]).status();
        // The backgrounded process stays alive after this shell exits, which the wait below
        // confirms; the OS has then already reparented it to init, off this test's own tree.
        let mut launcher = Command::new("sh")
            .arg("-c")
            .arg(format!("'{}' &", script.display()))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let _ = launcher.wait();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Ok(contents) = std::fs::read_to_string(&pidfile)
                && let Ok(pid) = contents.trim().parse()
            {
                return pid;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for the unrelated process's pid file"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}

/// Stops a process started by [`hold_unrelated_process`] by pid, since it isn't a `Child` this
/// process holds a handle to.
pub(crate) fn kill_unrelated_process(pid: u32) {
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .output();
    }
    #[cfg(not(windows))]
    {
        let _ = Command::new("kill").args(["-KILL", &pid.to_string()]).output();
    }
}

/// Writes the scripts [`grandchild_tick_command`] and [`grandchild_tick_shell_command`] both
/// start: one that keeps appending to a file forever, held by a wrapper that backgrounds it as a
/// grandchild and then itself hangs. Returns the wrapper to run and the file the grandchild keeps
/// touching.
fn write_grandchild_tick_scripts(dir: &Path) -> (PathBuf, PathBuf) {
    let ticks = dir.join("ticks.txt");
    #[cfg(windows)]
    {
        let tick_script = dir.join("tick.cmd");
        std::fs::write(
            &tick_script,
            format!(
                "@echo off\n:loop\n>>\"{}\" echo tick\nping -n 1 127.0.0.1 >nul\ngoto loop\n",
                ticks.display()
            ),
        )
        .unwrap();
        let hold_script = dir.join("hold_tick.cmd");
        std::fs::write(
            &hold_script,
            format!(
                "@echo off\nstart /B \"\" cmd /C call \"{}\"\nping -n 60 127.0.0.1 >nul\n",
                tick_script.display()
            ),
        )
        .unwrap();
        (hold_script, ticks)
    }
    #[cfg(not(windows))]
    {
        let script = dir.join("hold_tick.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n(while true; do date +%s%N >> '{}'; sleep 0.05; done) &\nsleep 60\n",
                ticks.display()
            ),
        )
        .unwrap();
        let _ = Command::new("chmod").args(["+x", script.to_str().unwrap()]).status();
        (script, ticks)
    }
}

/// A command that starts a grandchild which keeps appending to a file forever, so a kill that
/// only reaches the direct child (not the whole tree) would leave it running. Shared by
/// `process_tree.rs`'s and `engines/runner.rs`'s own tests that check a kill reaches the whole
/// tree, not just the process it was given. Returns the command to run and the file the
/// grandchild keeps touching; the caller still has to wait for the first tick before trusting the
/// grandchild has actually started.
pub(crate) fn grandchild_tick_command(dir: &Path) -> (PathBuf, Vec<String>, PathBuf) {
    let (hold_script, ticks) = write_grandchild_tick_scripts(dir);
    #[cfg(windows)]
    {
        (
            PathBuf::from("cmd"),
            vec![
                "/C".to_string(),
                "call".to_string(),
                hold_script.to_string_lossy().into_owned(),
            ],
            ticks,
        )
    }
    #[cfg(not(windows))]
    {
        (
            PathBuf::from("sh"),
            vec![hold_script.to_string_lossy().into_owned()],
            ticks,
        )
    }
}

/// The same grandchild-starting command as [`grandchild_tick_command`], but as a single shell
/// command string — what `proc::run_command_with_timeout` takes, instead of a `bin`/`args` pair.
pub(crate) fn grandchild_tick_shell_command(dir: &Path) -> (String, PathBuf) {
    let (hold_script, ticks) = write_grandchild_tick_scripts(dir);
    // No `"` and no space in the Windows command: `shell_command` passes it as a single
    // Windows command-line argument, and `Command` re-quotes and escapes any argument that
    // has either, which would mangle a quoted path before cmd.exe ever saw it.
    #[cfg(windows)]
    let command = hold_script.to_string_lossy().into_owned();
    #[cfg(not(windows))]
    let command = format!("sh \"{}\"", hold_script.display());
    (command, ticks)
}

pub(crate) fn git(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(dir);
    cmd
}

pub(crate) fn run(cmd: &mut Command) {
    let status = cmd.status().unwrap();
    assert!(status.success(), "{cmd:?} failed");
}

fn init_repo(dir: &Path, branch: &str) {
    run(git(dir).args(["init", "--quiet"]));
    run(git(dir).args(["config", "user.email", "test@example.com"]));
    run(git(dir).args(["config", "user.name", "Test"]));
    run(git(dir).args(["checkout", "-b", branch]));
    std::fs::write(dir.join("README.md"), "hello").unwrap();
    run(git(dir).args(["add", "README.md"]));
    run(git(dir).args(["commit", "--quiet", "-m", "initial"]));
}

/// A repo with an `origin` remote, both on `main`.
pub(crate) fn repo_with_origin(root: &Path) -> PathBuf {
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    init_repo(&repo, "main");
    let origin = root.join("origin.git");
    run(Command::new("git")
        .args(["clone", "--bare", "--quiet"])
        .arg(&repo)
        .arg(&origin));
    run(git(&repo).args(["remote", "add", "origin"]).arg(&origin));
    run(git(&repo).args(["push", "-u", "--quiet", "origin", "main"]));
    repo
}
