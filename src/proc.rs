//! Running a fixed command the orchestrator itself chose — never a model's — as a child process
//! in a checkout, outside any sandbox (docs/specs/agent/04-architecture.md section 4.2;
//! `agent/src/proc.ts` in the TypeScript agent). Shared by galleries' shoot command (06.8) and
//! stage 1's checks and npm install (06.3-review.md), which both need the same install-then-run
//! rule and the same capped, killable command runner.
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::lanes;
use crate::process_tree::{KILL_LIMIT, ProcessTree};
use crate::sync::lock;

/// How much of a running command's combined output is kept in memory at once.
const OUTPUT_CAP: usize = 20_000;
/// How long `npm ci`/`npm install` is given before it's treated as failed (06.8 bullet 9,
/// 06.3-review.md: "a 10-minute timeout").
pub const NPM_INSTALL_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// True when `dir` is a JS-based project whose dependencies are missing or stale: no
/// `node_modules/`, or `package-lock.json` modified more recently than
/// `node_modules/.package-lock.json` (06.8 bullet 9).
pub fn needs_npm_install(dir: &Path) -> bool {
    if !dir.join("package.json").exists() {
        return false;
    }
    if !dir.join("node_modules").is_dir() {
        return true;
    }
    let lock_mtime = std::fs::metadata(dir.join("package-lock.json")).and_then(|m| m.modified());
    let installed_mtime =
        std::fs::metadata(dir.join("node_modules").join(".package-lock.json")).and_then(|m| m.modified());
    match (lock_mtime, installed_mtime) {
        (Ok(lock), Ok(installed)) => lock > installed,
        (Ok(_), Err(_)) => true,
        _ => false,
    }
}

/// `npm ci` when there's a lockfile to install exactly from, else `npm install`.
pub fn npm_install_command(dir: &Path) -> &'static str {
    if dir.join("package-lock.json").exists() {
        "npm ci"
    } else {
        "npm install"
    }
}

/// Runs `command` in a shell under `cwd`, with no input, for up to `timeout`. Returns its exit
/// code (-1 if it couldn't be read), combined stdout/stderr (capped at [`OUTPUT_CAP`] bytes), and
/// whether it was killed for running too long. `PATH` is scrubbed of every other checkout's
/// `node_modules/.bin` first, with `cwd`'s own put first (06.8 bullet 10). The shell starts in its
/// own [`ProcessTree`] (07.4), so a timeout kills everything it spawned — a build, a test run, a
/// dev server — not just the shell itself; `lanes::kill_processes_in` then sweeps `cwd` for
/// anything that still left the tree.
pub fn run_command_with_timeout(command: &str, cwd: &Path, timeout: Duration) -> Result<(i32, String, bool)> {
    let mut command_builder = shell_command(command);
    command_builder
        .current_dir(cwd)
        .env("PATH", scrub_path(&path_env(), cwd))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    ProcessTree::prepare_sync(&mut command_builder);
    let mut child = command_builder
        .spawn()
        .with_context(|| format!("starting `{command}`"))?;
    let tree = ProcessTree::join_sync(&child).context("putting the command in its own process tree")?;
    let stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");
    let buf = std::sync::Arc::new(Mutex::new(Vec::<u8>::new()));
    let t_out = std::thread::spawn({
        let buf = buf.clone();
        move || capture_capped(stdout, buf)
    });
    let t_err = std::thread::spawn({
        let buf = buf.clone();
        move || capture_capped(stderr, buf)
    });
    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if Instant::now() >= deadline {
            timed_out = true;
            let _ = tree.kill_and_wait(KILL_LIMIT);
            let _ = lanes::kill_processes_in(cwd);
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    let _ = t_out.join();
    let _ = t_err.join();
    let output_bytes = lock(&buf).clone();
    let output = String::from_utf8_lossy(&output_bytes).into_owned();
    let code = status.and_then(|s| s.code()).unwrap_or(-1);
    Ok((code, output, timed_out))
}

#[cfg(windows)]
fn shell_command(command: &str) -> Command {
    let mut c = Command::new("cmd");
    c.args(["/C", command]);
    c
}

#[cfg(not(windows))]
fn shell_command(command: &str) -> Command {
    let mut c = Command::new("sh");
    c.args(["-c", command]);
    c
}

fn path_env() -> String {
    std::env::var("PATH").unwrap_or_default()
}

/// `path` with every entry ending in `node_modules/.bin` (another checkout's, left over from a
/// different project) dropped, and `dir`'s own `node_modules/.bin` put first (06.8 bullet 10:
/// "applies to the install too").
fn scrub_path(path: &str, dir: &Path) -> String {
    let own_bin = dir.join("node_modules").join(".bin");
    let kept = std::env::split_paths(path).filter(|p| !is_node_modules_bin(p));
    let entries = std::iter::once(own_bin).chain(kept);
    std::env::join_paths(entries)
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string())
}

fn is_node_modules_bin(p: &Path) -> bool {
    p.file_name().is_some_and(|f| f == ".bin")
        && p.parent()
            .and_then(|p| p.file_name())
            .is_some_and(|f| f == "node_modules")
}

/// Reads `pipe` to EOF into `buf`, kept to the last [`OUTPUT_CAP`] bytes, like the TypeScript
/// agent's `output.slice(-20_000)` while a command is still running.
fn capture_capped(mut pipe: impl Read, buf: std::sync::Arc<Mutex<Vec<u8>>>) {
    let mut chunk = [0u8; 4096];
    loop {
        let n = match pipe.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        let mut buf = lock(&buf);
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > OUTPUT_CAP {
            let drop = buf.len() - OUTPUT_CAP;
            buf.drain(..drop);
        }
    }
}

/// The last `n` lines of `text`.
pub fn tail_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Waits for `path` to exist and be non-empty, so a test only starts timing a kill once the
    /// grandchild it's watching has actually ticked at least once.
    fn wait_for_first_tick(path: &Path) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if std::fs::metadata(path).map(|m| m.len() > 0).unwrap_or(false) {
                return;
            }
            assert!(Instant::now() < deadline, "the grandchild never ticked");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn run_command_with_timeout_kills_the_whole_tree_including_a_grandchild() {
        let dir = tempfile::tempdir().unwrap();
        let (command, ticks) = crate::test_support::grandchild_tick_shell_command(dir.path());
        let cwd = dir.path().to_path_buf();
        // `run_command_with_timeout` blocks until it times out, so the grandchild it starts is
        // only watchable from another thread.
        let call = std::thread::spawn(move || run_command_with_timeout(&command, &cwd, Duration::from_millis(300)));

        wait_for_first_tick(&ticks);
        let size_before_timeout = std::fs::metadata(&ticks).unwrap().len();

        let (_, _, timed_out) = call.join().unwrap().unwrap();

        assert!(timed_out);
        let size_right_after = std::fs::metadata(&ticks).unwrap().len();
        std::thread::sleep(Duration::from_millis(400));
        let size_later = std::fs::metadata(&ticks).unwrap().len();
        assert!(size_right_after >= size_before_timeout);
        assert_eq!(
            size_right_after, size_later,
            "the grandchild kept touching its file after the timeout"
        );
    }

    #[test]
    fn needs_npm_install_is_false_without_a_package_json() {
        let root = tempfile::tempdir().unwrap();
        assert!(!needs_npm_install(root.path()));
    }

    #[test]
    fn needs_npm_install_is_true_when_node_modules_is_missing() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("package.json"), "{}").unwrap();
        assert!(needs_npm_install(root.path()));
    }

    #[test]
    fn needs_npm_install_is_false_once_node_modules_is_up_to_date() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("package.json"), "{}").unwrap();
        std::fs::write(root.path().join("package-lock.json"), "{}").unwrap();
        std::fs::create_dir_all(root.path().join("node_modules")).unwrap();
        std::fs::write(root.path().join("node_modules").join(".package-lock.json"), "{}").unwrap();
        let now = std::time::SystemTime::now();
        set_mtime(
            &root.path().join("package-lock.json"),
            now - std::time::Duration::from_secs(60),
        );
        set_mtime(&root.path().join("node_modules").join(".package-lock.json"), now);

        assert!(!needs_npm_install(root.path()));
    }

    fn set_mtime(path: &Path, when: std::time::SystemTime) {
        std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }

    #[test]
    fn needs_npm_install_is_true_once_the_lockfile_is_newer_than_the_installed_copy() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("package.json"), "{}").unwrap();
        std::fs::write(root.path().join("package-lock.json"), "{}").unwrap();
        std::fs::create_dir_all(root.path().join("node_modules")).unwrap();
        std::fs::write(root.path().join("node_modules").join(".package-lock.json"), "{}").unwrap();
        let now = std::time::SystemTime::now();
        set_mtime(
            &root.path().join("node_modules").join(".package-lock.json"),
            now - std::time::Duration::from_secs(60),
        );
        set_mtime(&root.path().join("package-lock.json"), now);

        assert!(needs_npm_install(root.path()));
    }

    #[test]
    fn npm_install_command_prefers_ci_when_theres_a_lockfile() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("package-lock.json"), "{}").unwrap();
        assert_eq!(npm_install_command(root.path()), "npm ci");
    }

    #[test]
    fn npm_install_command_falls_back_to_install_without_a_lockfile() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(npm_install_command(root.path()), "npm install");
    }

    #[test]
    fn scrub_path_drops_every_node_modules_bin_and_puts_this_checkouts_first() {
        let dir = Path::new("checkout");
        let other_sep = if cfg!(windows) { ';' } else { ':' };
        let other = std::env::join_paths([
            Path::new("checkout").join("node_modules").join(".bin"),
            Path::new("other-checkout").join("node_modules").join(".bin"),
            Path::new("usr").join("bin"),
        ])
        .unwrap()
        .to_string_lossy()
        .into_owned();

        let scrubbed = scrub_path(&other, dir);

        let parts: Vec<&str> = scrubbed.split(other_sep).collect();
        assert_eq!(parts[0], dir.join("node_modules").join(".bin").to_string_lossy());
        assert_eq!(parts.len(), 2, "{scrubbed}");
        assert!(parts[1].ends_with("bin"), "{scrubbed}");
    }

    #[test]
    fn tail_lines_keeps_only_the_last_n() {
        let text = "a\nb\nc\nd";
        assert_eq!(tail_lines(text, 2), "c\nd");
        assert_eq!(tail_lines(text, 10), text);
    }
}
