//! The pool of git worktrees ("lanes") a project's sessions work in, one at a time
//! (docs/specs/agent/04-architecture.md section 4.1). A lane is `<worktreesDir>/lane-<k>`
//! (k = 1..=the computer's `lanes`), made once with `git worktree add --detach` and kept
//! between items so its build output stays warm; the orchestrator never makes a fresh worktree
//! per item. `ensure` reconciles the worktrees on disk for one project to the wanted count.
//! `prepare` resets a lane onto a fresh branch from the base (fetch, reset, clean, checkout -B),
//! after stopping anything a previous session left running there, and links sibling path
//! dependencies that climb out of the repo. Handing a lane to one session at a time is for a
//! later piece, once there's a session to hand one to.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

/// How recently a fetch of a repo must have begun for the next lane's prepare to skip its own
/// (section 4.1: every lane shares the one object store).
const FRESH_FETCH: Duration = Duration::from_secs(15);

/// Serialises fetches and branch moves that write the shared object store.
static GIT_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

/// When each repo's last fetch began, so a prepare within [`FRESH_FETCH`] of it can skip.
static FETCHED_AT: LazyLock<Mutex<HashMap<PathBuf, Instant>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// What `ensure` changed, for logging.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Reconciled {
    pub created: Vec<u32>,
    pub removed: Vec<u32>,
}

/// Reconciles `<worktrees_dir>/lane-<k>` for k = 1..=`lane_count` to exist as worktrees of
/// `repo_path`, creating what's missing and removing anything numbered higher than that
/// ("raising lanes adds worktrees on demand. Lowering it removes the highest-numbered lanes once
/// they're free"). A lane folder left behind with no `.git` (a worktree remove that failed
/// part-way, e.g. a locked file on Windows) is deleted before a lane is made there again
/// ("No half-removed lanes"). Safe to call repeatedly: an already-reconciled pool is left as it
/// is.
pub fn ensure(repo_path: &Path, worktrees_dir: &Path, lane_count: u32) -> Result<Reconciled> {
    std::fs::create_dir_all(worktrees_dir).with_context(|| format!("creating {}", worktrees_dir.display()))?;
    let mut result = Reconciled::default();
    for lane in 1..=lane_count {
        let path = lane_path(worktrees_dir, lane);
        if is_worktree(&path) {
            continue;
        }
        if path.exists() {
            let _ = kill_processes_in(&path);
            std::fs::remove_dir_all(&path).with_context(|| format!("removing {}", path.display()))?;
        }
        add_worktree(repo_path, &path)?;
        link_outside_deps(repo_path, worktrees_dir, &path)?;
        result.created.push(lane);
    }
    for lane in existing_lane_numbers(worktrees_dir)? {
        if lane <= lane_count {
            continue;
        }
        let path = lane_path(worktrees_dir, lane);
        if is_worktree(&path) {
            let _ = kill_processes_in(&path);
            if let Err(e) = remove_worktree(repo_path, &path) {
                tracing::warn!("removing lane {lane} at {}: {e:#}", path.display());
                continue;
            }
        } else {
            let _ = kill_processes_in(&path);
            std::fs::remove_dir_all(&path).with_context(|| format!("removing {}", path.display()))?;
        }
        result.removed.push(lane);
    }
    Ok(result)
}

/// Clears whatever a lane's last run left, then resets it onto a fresh `branch` from
/// `origin/<base_branch>`. `clean -fd` without `-x` keeps ignored build output. Fetch is skipped
/// when one of this repo began less than 15 seconds ago. Returns the lane's directory.
pub fn prepare(repo_path: &Path, worktrees_dir: &Path, lane: u32, branch: &str, base_branch: &str) -> Result<PathBuf> {
    let dir = lane_path(worktrees_dir, lane);
    ensure_worktree(repo_path, worktrees_dir, &dir)?;
    kill_processes_in(&dir)?;
    {
        let _guard = GIT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        fetch(repo_path, true)?;
        run(git_in_worktree(&dir).args(["reset", "--hard"]))?;
        run(git_in_worktree(&dir).args(["clean", "-fd"]))?;
        let start_point = format!("origin/{base_branch}");
        run(git_in_worktree(&dir).args(["checkout", "-B", branch, &start_point]))?;
    }
    // After clean: junctions created before it are gone; put them back for this item.
    link_outside_deps(repo_path, worktrees_dir, &dir)?;
    Ok(dir)
}

/// Something just changed origin without a fetch here (a PR merged on GitHub): the next lane
/// fetches again.
pub fn fetch_is_stale(repo_path: &Path) {
    let key = std::fs::canonicalize(repo_path).unwrap_or_else(|_| repo_path.to_path_buf());
    FETCHED_AT.lock().unwrap_or_else(|e| e.into_inner()).remove(&key);
}

/// Stops every process whose program or command line is inside `dir`, with its children. A session
/// can leave a server running (`wrangler dev`, `vite`) that outlives it and, on Windows, holds
/// files the clean can't remove. Returns how many it stopped.
pub fn kill_processes_in(dir: &Path) -> Result<usize> {
    let pids = processes_in(dir)?;
    for pid in &pids {
        kill_tree(*pid)?;
    }
    Ok(pids.len())
}

/// Relative dependency paths in a Cargo.toml or package.json that climb out of the file's folder
/// (`path = "../…"`, `"file:../…"`, `"link:../…"`).
pub fn outside_paths(manifest: &str) -> Vec<String> {
    let mut found = Vec::new();
    for path in path_eq_values(manifest)
        .into_iter()
        .chain(file_or_link_values(manifest))
    {
        if path.starts_with("..") && !Path::new(&path).is_absolute() && !found.iter().any(|p| p == &path) {
            found.push(path);
        }
    }
    found
}

/// `<worktrees_dir>/lane-<lane>`.
pub fn lane_path(worktrees_dir: &Path, lane: u32) -> PathBuf {
    worktrees_dir.join(format!("lane-{lane}"))
}

fn ensure_worktree(repo_path: &Path, worktrees_dir: &Path, dir: &Path) -> Result<()> {
    if !is_worktree(dir) {
        if dir.exists() {
            let _ = kill_processes_in(dir);
            std::fs::remove_dir_all(dir).with_context(|| format!("removing {}", dir.display()))?;
        }
        if let Some(parent) = dir.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        add_worktree(repo_path, dir)?;
    }
    link_outside_deps(repo_path, worktrees_dir, dir)?;
    Ok(())
}

/// Repos often point at siblings (`path = "../void-engine"`). From a lane those relative paths
/// land next to the lane; this adds a directory junction (Windows) or symlink (Unix) where the
/// lane expects each one, pointing at the real folder. Links are only created under
/// `worktrees_dir` itself (04-architecture.md section 4.1: "It only creates links under the
/// worktrees root"), so a manifest that climbs out further than one sibling level can't place a
/// link outside it.
fn link_outside_deps(repo_path: &Path, worktrees_dir: &Path, dir: &Path) -> Result<()> {
    let repo = PathBuf::from(norm_path(
        &std::fs::canonicalize(repo_path).unwrap_or_else(|_| repo_path.to_path_buf()),
    ));
    let root = PathBuf::from(norm_path(
        &std::fs::canonicalize(worktrees_dir).unwrap_or_else(|_| worktrees_dir.to_path_buf()),
    ));
    let listed = git_output_in_worktree(
        dir,
        &[
            "ls-files",
            "--",
            "Cargo.toml",
            "*/Cargo.toml",
            "package.json",
            "*/package.json",
        ],
    )?;
    for file in listed.lines().map(str::trim).filter(|s| !s.is_empty()) {
        if file.contains("node_modules/") {
            continue;
        }
        let manifest_path = file.split('/').fold(dir.to_path_buf(), |p, part| p.join(part));
        let text = match std::fs::read_to_string(&manifest_path) {
            Ok(t) => t,
            Err(_) => continue,
        };
        for rel in outside_paths(&text) {
            let file_dir = Path::new(file).parent().unwrap_or_else(|| Path::new(""));
            let target_raw = PathBuf::from(norm_path(&repo)).join(file_dir).join(&rel);
            let target = PathBuf::from(norm_path(
                &std::fs::canonicalize(&target_raw).unwrap_or_else(|_| target_raw.clone()),
            ));
            // Only siblings outside the repo; skip missing targets and anything inside it.
            if target == repo || is_under(&target, &repo) || !target.exists() {
                continue;
            }
            let link = PathBuf::from(norm_path(
                &std::path::absolute(dir.join(file_dir).join(&rel)).unwrap_or_else(|_| dir.join(file_dir).join(&rel)),
            ));
            if !is_under(&link, &root) || link.exists() {
                continue;
            }
            if let Some(parent) = link.parent() {
                std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
            }
            junction_or_symlink(&target, &link)?;
        }
    }
    Ok(())
}

fn is_under(path: &Path, root: &Path) -> bool {
    let path = norm_path(path);
    let root = norm_path(root);
    #[cfg(windows)]
    let (path, root) = (path.to_lowercase(), root.to_lowercase());
    let path = Path::new(&path);
    let root = Path::new(&root);
    match path.strip_prefix(root) {
        Ok(rest) => !rest.as_os_str().is_empty(),
        Err(_) => false,
    }
}

/// Drop Windows `\\?\` / `\\?\UNC\` prefixes so canonicalize'd paths compare and match process
/// command lines (which never carry them).
fn norm_path(path: &Path) -> String {
    let s = path.to_string_lossy();
    #[cfg(windows)]
    {
        s.strip_prefix(r"\\?\UNC\")
            .map(|r| format!(r"\\{r}"))
            .unwrap_or_else(|| s.strip_prefix(r"\\?\").unwrap_or(&s).to_string())
    }
    #[cfg(not(windows))]
    {
        s.into_owned()
    }
}

fn junction_or_symlink(target: &Path, link: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        // A directory junction needs no elevation (unlike a directory symlink). mklink wants
        // ordinary paths, not the `\\?\` form canonicalize returns.
        let link = norm_path(link);
        let target = norm_path(target);
        let status = Command::new("cmd")
            .args(["/C", "mklink", "/J", &link, &target])
            .output()
            .context("creating directory junction")?;
        if !status.status.success() {
            bail!("mklink /J failed: {}", String::from_utf8_lossy(&status.stderr).trim());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        std::os::unix::fs::symlink(target, link)
            .with_context(|| format!("symlink {} -> {}", link.display(), target.display()))
    }
}

fn fetch(repo_path: &Path, unless_fresh: bool) -> Result<bool> {
    let key = std::fs::canonicalize(repo_path).unwrap_or_else(|_| repo_path.to_path_buf());
    if unless_fresh {
        let map = FETCHED_AT.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(at) = map.get(&key)
            && at.elapsed() < FRESH_FETCH
        {
            return Ok(false);
        }
    }
    let started = Instant::now();
    run(git(repo_path).args(["fetch", "--prune", "origin"]))?;
    FETCHED_AT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, started);
    Ok(true)
}

fn is_worktree(path: &Path) -> bool {
    path.join(".git").exists()
}

fn existing_lane_numbers(worktrees_dir: &Path) -> Result<Vec<u32>> {
    let mut found = Vec::new();
    let entries = match std::fs::read_dir(worktrees_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(found),
        Err(e) => return Err(e).with_context(|| format!("reading {}", worktrees_dir.display())),
    };
    for entry in entries {
        let entry = entry?;
        if let Some(lane) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.strip_prefix("lane-"))
            .and_then(|num| num.parse::<u32>().ok())
        {
            found.push(lane);
        }
    }
    found.sort_unstable();
    Ok(found)
}

fn git(repo_path: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(repo_path);
    cmd
}

/// A git call inside `<worktreesDir>` (a lane, in this module). These worktrees share the
/// project's object store, which keeps its own upkeep, so the agent's own calls here skip
/// `maintenance run --auto` (section 4.1.1: "one more process each time, to decide there's
/// nothing to do").
fn git_in_worktree(dir: &Path) -> Command {
    let mut cmd = git(dir);
    cmd.args(["-c", "maintenance.auto=false", "-c", "gc.auto=0"]);
    cmd
}

fn run(cmd: &mut Command) -> Result<()> {
    let output = cmd.output().context("running git")?;
    if !output.status.success() {
        bail!("git failed: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(())
}

fn git_output_in_worktree(dir: &Path, args: &[&str]) -> Result<String> {
    let output = git_in_worktree(dir).args(args).output().context("running git")?;
    if !output.status.success() {
        bail!("git failed: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn add_worktree(repo_path: &Path, lane_path: &Path) -> Result<()> {
    run(git(repo_path).args(["worktree", "add", "--detach"]).arg(lane_path))
}

fn remove_worktree(repo_path: &Path, lane_path: &Path) -> Result<()> {
    run(git(repo_path).args(["worktree", "remove", "--force"]).arg(lane_path))
}

fn path_eq_values(manifest: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = manifest.as_bytes();
    let mut i = 0;
    while i + 4 <= bytes.len() {
        if &bytes[i..i + 4] == b"path" && word_boundary_before(bytes, i) {
            let after = skip_ws(&manifest[i + 4..]);
            if let Some(after) = after.strip_prefix('=') {
                let after = skip_ws(after);
                if let Some(value) = quoted_string(after) {
                    out.push(value.to_string());
                }
            }
        }
        i += 1;
    }
    out
}

fn file_or_link_values(manifest: &str) -> Vec<String> {
    let mut out = Vec::new();
    for prefix in ["file:", "link:"] {
        let mut rest = manifest;
        while let Some(idx) = rest.find(prefix) {
            if idx > 0 && rest.as_bytes()[idx - 1] == b'"' {
                let after = &rest[idx + prefix.len()..];
                if let Some(end) = after.find('"') {
                    out.push(after[..end].to_string());
                }
            }
            rest = &rest[idx + prefix.len()..];
        }
    }
    out
}

fn word_boundary_before(bytes: &[u8], i: usize) -> bool {
    i == 0 || !bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'_'
}

fn skip_ws(s: &str) -> &str {
    s.trim_start_matches(|c: char| c.is_whitespace())
}

fn quoted_string(s: &str) -> Option<&str> {
    let s = s.strip_prefix('"')?;
    let end = s.find('"')?;
    Some(&s[..end])
}

fn processes_in(dir: &Path) -> Result<Vec<u32>> {
    // Command lines may use either the path as given or its resolved form: on macOS, /var and
    // /tmp are symlinks to /private/..., so a process started as `/var/folders/.../lane-1/...`
    // never matches a root resolved to `/private/var/folders/.../lane-1/`.
    let given = root_str(dir);
    let resolved_root = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let resolved = root_str(&resolved_root);
    let self_pid = std::process::id();
    let stdout = list_processes()?;
    let mut pids = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (pid_str, rest) = match line.split_once(|c: char| c.is_whitespace()) {
            Some(pair) => pair,
            None => continue,
        };
        let pid: u32 = match pid_str.parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        if pid == self_pid {
            continue;
        }
        // Command lines use ordinary paths; normalize so a `\\?\` root still matches.
        let hay = {
            #[cfg(windows)]
            {
                norm_path(Path::new(rest)).to_lowercase()
            }
            #[cfg(not(windows))]
            {
                rest.to_string()
            }
        };
        if hay.contains(&given) || hay.contains(&resolved) {
            pids.push(pid);
        }
    }
    Ok(pids)
}

/// Normalizes `path` to compare against a `ps`/CIM command line: drops a Windows `\\?\` prefix,
/// adds a trailing separator so `lane-1` doesn't match `lane-10`, and lowercases on Windows
/// (case-insensitive paths).
fn root_str(path: &Path) -> String {
    let mut s = norm_path(path);
    if !s.ends_with(std::path::MAIN_SEPARATOR) {
        s.push(std::path::MAIN_SEPARATOR);
    }
    #[cfg(windows)]
    {
        s = s.to_lowercase();
    }
    s
}

fn list_processes() -> Result<String> {
    #[cfg(windows)]
    {
        let output = Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                r#"Get-CimInstance Win32_Process | ForEach-Object { "$($_.ProcessId)`t$($_.ExecutablePath) $($_.CommandLine)" }"#,
            ])
            .output()
            .context("listing processes")?;
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
    #[cfg(not(windows))]
    {
        let output = Command::new("ps")
            .args(["-axo", "pid=,args="])
            .output()
            .context("listing processes")?;
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

fn kill_tree(pid: u32) -> Result<()> {
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/pid", &pid.to_string(), "/T", "/F"])
            .output();
        Ok(())
    }
    #[cfg(not(windows))]
    {
        // `kill` alone only stops the matched pid: a child (e.g. a `node` worker started with
        // relative args) rarely carries the lane path on its own command line, so walk the
        // process tree and stop its descendants too, to match `taskkill /T` on Windows.
        for p in std::iter::once(pid).chain(descendants_of(pid)) {
            let _ = Command::new("kill").args(["-TERM", &p.to_string()]).output();
        }
        Ok(())
    }
}

#[cfg(not(windows))]
fn descendants_of(pid: u32) -> Vec<u32> {
    let output = match Command::new("ps").args(["-axo", "pid=,ppid="]).output() {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for line in stdout.lines() {
        let mut parts = line.split_whitespace();
        let (Some(child), Some(parent)) = (parts.next(), parts.next()) else {
            continue;
        };
        let (Ok(child), Ok(parent)) = (child.parse::<u32>(), parent.parse::<u32>()) else {
            continue;
        };
        children.entry(parent).or_default().push(child);
    }
    let mut out = Vec::new();
    let mut queue = vec![pid];
    while let Some(current) = queue.pop() {
        if let Some(kids) = children.get(&current) {
            for &kid in kids {
                out.push(kid);
                queue.push(kid);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;

    /// A bare repo with one commit, so `git worktree add` has a HEAD to check out.
    fn init_repo(dir: &Path) {
        run(git(dir).args(["init", "--quiet"])).unwrap();
        run(git(dir).args(["config", "user.email", "test@example.com"])).unwrap();
        run(git(dir).args(["config", "user.name", "Test"])).unwrap();
        // Default branch name so origin/<base> matches across git versions.
        run(git(dir).args(["checkout", "-b", "main"])).unwrap();
        std::fs::write(dir.join("README.md"), "hello").unwrap();
        run(git(dir).args(["add", "README.md"])).unwrap();
        run(git(dir).args(["commit", "--quiet", "-m", "initial"])).unwrap();
    }

    /// Repo with an `origin` remote so prepare can fetch and check out `origin/main`.
    fn init_repo_with_origin(root: &Path) -> PathBuf {
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        init_repo(&repo);
        let origin = root.join("origin.git");
        run(Command::new("git")
            .args(["clone", "--bare", "--quiet"])
            .arg(&repo)
            .arg(&origin))
        .unwrap();
        run(git(&repo).args(["remote", "add", "origin"]).arg(&origin)).unwrap();
        run(git(&repo).args(["push", "-u", "--quiet", "origin", "main"])).unwrap();
        repo
    }

    fn worktree_list(repo: &Path) -> String {
        let output = git(repo).args(["worktree", "list"]).output().unwrap();
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn branch_of(dir: &Path) -> String {
        git_output_in_worktree(dir, &["branch", "--show-current"])
            .unwrap()
            .trim()
            .to_string()
    }

    #[test]
    fn ensure_creates_the_wanted_number_of_lanes() {
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        let worktrees = tempfile::tempdir().unwrap();

        let result = ensure(repo.path(), worktrees.path(), 3).unwrap();

        assert_eq!(result.created, vec![1, 2, 3]);
        assert!(result.removed.is_empty());
        for lane in 1..=3 {
            assert!(is_worktree(&lane_path(worktrees.path(), lane)));
        }
        assert_eq!(worktree_list(repo.path()).lines().count(), 4); // the repo itself plus 3 lanes
    }

    #[test]
    fn ensure_is_a_no_op_once_the_pool_already_matches() {
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        let worktrees = tempfile::tempdir().unwrap();
        ensure(repo.path(), worktrees.path(), 2).unwrap();

        let second = ensure(repo.path(), worktrees.path(), 2).unwrap();

        assert_eq!(second, Reconciled::default());
    }

    #[test]
    fn lowering_the_count_removes_the_highest_numbered_lanes_once_theyre_free() {
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        let worktrees = tempfile::tempdir().unwrap();
        ensure(repo.path(), worktrees.path(), 4).unwrap();

        let result = ensure(repo.path(), worktrees.path(), 2).unwrap();

        assert_eq!(result.removed, vec![3, 4]);
        assert!(!lane_path(worktrees.path(), 3).exists());
        assert!(!lane_path(worktrees.path(), 4).exists());
        assert!(lane_path(worktrees.path(), 1).exists());
        assert!(lane_path(worktrees.path(), 2).exists());
    }

    #[test]
    fn raising_the_count_again_recreates_the_lanes_it_had_removed() {
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        let worktrees = tempfile::tempdir().unwrap();
        ensure(repo.path(), worktrees.path(), 3).unwrap();
        ensure(repo.path(), worktrees.path(), 1).unwrap();

        let result = ensure(repo.path(), worktrees.path(), 3).unwrap();

        assert_eq!(result.created, vec![2, 3]);
        assert!(is_worktree(&lane_path(worktrees.path(), 2)));
        assert!(is_worktree(&lane_path(worktrees.path(), 3)));
    }

    #[test]
    fn a_half_removed_lane_folder_is_deleted_before_its_lane_is_made_again() {
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        let worktrees = tempfile::tempdir().unwrap();
        let stray = lane_path(worktrees.path(), 2);
        std::fs::create_dir_all(&stray).unwrap();
        std::fs::write(stray.join("leftover.txt"), "junk").unwrap();

        let result = ensure(repo.path(), worktrees.path(), 2).unwrap();

        assert_eq!(result.created, vec![1, 2]);
        assert!(is_worktree(&stray));
        assert!(!stray.join("leftover.txt").exists());
    }

    #[test]
    fn a_zero_count_removes_every_lane() {
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        let worktrees = tempfile::tempdir().unwrap();
        ensure(repo.path(), worktrees.path(), 2).unwrap();

        let result = ensure(repo.path(), worktrees.path(), 0).unwrap();

        assert_eq!(result.removed, vec![1, 2]);
        assert_eq!(worktree_list(repo.path()).lines().count(), 1);
    }

    #[test]
    fn outside_paths_finds_dependency_paths_that_climb_out_of_the_repo() {
        let cargo = "[dependencies]\nvoid_engine = { path = \"../void-engine\" }\nlocal = { path = \"crates/local\" }\nserde = \"1\"";
        assert_eq!(outside_paths(cargo), vec!["../void-engine"]);
        let pkg =
            r#"{"dependencies": {"a": "file:../lib-a", "b": "link:../../b", "c": "^1.0.0", "d": "file:./inside"}}"#;
        assert_eq!(outside_paths(pkg), vec!["../lib-a", "../../b"]);
        // A multi-byte character before the dependency must not panic (byte vs. char indexing).
        let non_ascii =
            "[package]\nauthors = [\"José 🚀\"]\n\n[dependencies]\nvoid_engine = { path = \"../void-engine\" }\n";
        assert_eq!(outside_paths(non_ascii), vec!["../void-engine"]);
    }

    #[test]
    fn prepare_resets_a_lane_onto_a_fresh_branch_from_the_base() {
        let root = tempfile::tempdir().unwrap();
        let repo = init_repo_with_origin(root.path());
        let worktrees = root.path().join("worktrees");
        ensure(&repo, &worktrees, 1).unwrap();
        // Dirt the lane so prepare has something to clean.
        let lane = lane_path(&worktrees, 1);
        std::fs::write(lane.join("dirt.txt"), "gone").unwrap();
        std::fs::write(lane.join("README.md"), "edited").unwrap();

        let dir = prepare(&repo, &worktrees, 1, "retaskable/item-slug-s1", "main").unwrap();

        assert_eq!(dir, lane);
        assert_eq!(branch_of(&lane), "retaskable/item-slug-s1");
        assert!(!lane.join("dirt.txt").exists());
        assert_eq!(std::fs::read_to_string(lane.join("README.md")).unwrap(), "hello");
    }

    #[test]
    fn prepare_adds_a_junction_for_a_sibling_path_dependency() {
        let root = tempfile::tempdir().unwrap();
        let sibling = root.path().join("void-engine");
        std::fs::create_dir_all(&sibling).unwrap();
        std::fs::write(sibling.join("lib.rs"), "// engine").unwrap();
        let repo = init_repo_with_origin(root.path());
        std::fs::write(
            repo.join("Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nvoid_engine = { path = \"../void-engine\" }\n",
        )
        .unwrap();
        run(git(&repo).args(["add", "Cargo.toml"])).unwrap();
        run(git(&repo).args(["commit", "--quiet", "-m", "dep"])).unwrap();
        run(git(&repo).args(["push", "--quiet", "origin", "main"])).unwrap();
        let worktrees = root.path().join("worktrees");

        prepare(&repo, &worktrees, 1, "retaskable/dep-s1", "main").unwrap();

        let link = worktrees.join("void-engine");
        assert!(link.exists(), "junction/symlink at {}", link.display());
        assert!(link.join("lib.rs").exists(), "link should resolve to the real sibling");
    }

    #[test]
    fn a_dependency_that_climbs_out_past_the_worktrees_root_gets_no_link() {
        // "It only creates links under the worktrees root" (04-architecture.md section 4.1): a
        // manifest that climbs out by more than one sibling level would, without that boundary,
        // land its junction next to the worktrees root instead of inside it.
        let outer = tempfile::tempdir().unwrap();
        let proj = outer.path().join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        let too_far = outer.path().join("too-far-sibling");
        std::fs::create_dir_all(&too_far).unwrap();
        std::fs::write(too_far.join("lib.rs"), "// too far").unwrap();
        let repo = init_repo_with_origin(&proj);
        std::fs::write(
            repo.join("Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\ntoo_far = { path = \"../../too-far-sibling\" }\n",
        )
        .unwrap();
        run(git(&repo).args(["add", "Cargo.toml"])).unwrap();
        run(git(&repo).args(["commit", "--quiet", "-m", "dep"])).unwrap();
        run(git(&repo).args(["push", "--quiet", "origin", "main"])).unwrap();
        let worktrees = proj.join("worktrees");

        prepare(&repo, &worktrees, 1, "retaskable/far-s1", "main").unwrap();

        assert!(
            !proj.join("too-far-sibling").exists(),
            "no junction should land next to the worktrees root"
        );
        assert!(!worktrees.join("too-far-sibling").exists());
    }

    #[test]
    fn a_process_left_running_from_inside_a_lane_is_stopped_and_one_outside_it_isnt() {
        let root = tempfile::tempdir().unwrap();
        let lane = root.path().join("lane-1");
        // lane-10's path string starts with lane-1's; matching must use a trailing separator.
        let other = root.path().join("lane-10");
        std::fs::create_dir_all(&lane).unwrap();
        std::fs::create_dir_all(&other).unwrap();

        let mut in_lane = hold_process(&lane);
        let mut outside = hold_process(&other);
        // Wait until the OS lists the in-lane process (CIM can lag on a busy machine).
        let deadline = Instant::now() + Duration::from_secs(10);
        while processes_in(&lane).unwrap().is_empty() {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for the in-lane process to appear in the process list"
            );
            std::thread::sleep(Duration::from_millis(200));
        }

        let stopped = kill_processes_in(&lane).unwrap();
        assert!(stopped >= 1, "expected to stop the in-lane process, stopped {stopped}");
        let _ = in_lane.wait();
        assert!(outside.try_wait().unwrap().is_none(), "lane-10 keeps running");
        let _ = outside.kill();
        let _ = outside.wait();
    }

    /// Starts a long-lived process whose command line contains `dir`, so `processes_in` finds it.
    fn hold_process(dir: &Path) -> std::process::Child {
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
}
