//! "The repo" (docs/specs/agent/09-web-ui.md section 9.1), ported from `repoInfo`, `githubLogin`,
//! `devBranch`, `createBranchIfMissing` (`agent/src/setup.ts`) and `listOwners`/`listRepos`/
//! `remoteRepo` (`agent/src/github.ts`). A local folder, or one of the person's (and their GitHub
//! organisations') `gh repo list` repos, or, on GitLab, the person's and their groups' `glab repo
//! list` repos (`repo_gitlab`). Telling GitHub, GitLab and plain git remotes apart (`code_host`)
//! is ported, and `repo_info`, `list_owners`, `list_repos` and `clone_repo` use `gh` or `glab` to
//! match. A plain remote is handled with git alone (`repo_info` reads its default branch with `git
//! ls-remote --symref`, `remote_reachable` checks it, `clone_remote` clones it, `find_checkout`
//! matches it by its normalised URL), and it has no owner list, so listing it is an error. A
//! local-only project (a git repo with no remote at all: `has_remote`, `local_default_branch`,
//! `local_branch_exists`, `RepoInfo::local_only`) is ported from `suggest`/`validate`; the full
//! `GithubPicker`'s clone-serialising lock isn't (its caching is `repo_picker`).
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{LazyLock, Mutex};

pub use crate::repo_local::{has_remote, local_branch_exists, local_default_branch};

use anyhow::{Context, Result, bail};
use regex::Regex;
use serde::Deserialize;

/// `gh repo view`'s answer about a checkout's GitHub identity (`repoInfo` in `setup.ts`). Every
/// field is `None` when `gh` can't answer: no GitHub remote, `gh` not authenticated, or no repo
/// there at all.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoInfo {
    /// `origin`'s URL and the host it's on; `None` without an origin.
    pub origin_url: Option<String>,
    pub code_host: Option<CodeHost>,
    /// The project on its code host: the GitHub `owner/name` or the GitLab `path_with_namespace`.
    pub repo: Option<String>,
    /// `repo` when `code_host` is GitHub, `None` otherwise.
    pub github_repo: Option<String>,
    pub default_branch: Option<String>,
    pub upstream: Option<Upstream>,
    /// A git repo with no remote at all: the work stays on this computer.
    pub local_only: bool,
}

/// A fork's parent and its default branch (github-workflow.md section 4.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upstream {
    pub repo: String,
    pub default_branch: Option<String>,
}

#[derive(Deserialize)]
struct RepoViewJson {
    #[serde(rename = "nameWithOwner")]
    name_with_owner: Option<String>,
    #[serde(rename = "defaultBranchRef")]
    default_branch_ref: Option<BranchRef>,
    #[serde(rename = "isFork")]
    is_fork: Option<bool>,
    parent: Option<Parent>,
}

#[derive(Deserialize)]
struct BranchRef {
    name: Option<String>,
}

#[derive(Deserialize)]
struct Parent {
    name: Option<String>,
    owner: Option<Owner>,
}

#[derive(Deserialize)]
struct Owner {
    login: Option<String>,
}

#[derive(Deserialize)]
struct DefaultBranchOnly {
    #[serde(rename = "defaultBranchRef")]
    default_branch_ref: Option<BranchRef>,
}

/// The checkout's `origin`, project, default branch and (for a fork) upstream: `gh repo view` on
/// GitHub, `glab repo view` on GitLab, and just the origin for a plain remote.
pub fn repo_info(repo_path: &Path) -> RepoInfo {
    let Some(url) = origin_url(repo_path) else {
        if !has_remote(repo_path) {
            return RepoInfo {
                local_only: true,
                default_branch: local_default_branch(repo_path),
                ..RepoInfo::default()
            };
        }
        return RepoInfo::default();
    };
    let host = code_host(&url);
    let mut info = match host {
        CodeHost::Github => github_repo_info(repo_path),
        CodeHost::Gitlab => crate::repo_gitlab::repo_info("glab", repo_path),
        CodeHost::Plain => RepoInfo {
            default_branch: plain_default_branch(repo_path),
            ..RepoInfo::default()
        },
    };
    info.origin_url = Some(url);
    info.code_host = Some(host);
    info
}

/// Runs `gh` (in `dir` when given) unless an argument is undefined ([`crate::host_call::guard_program`]).
fn gh(dir: Option<&Path>, args: &[&str]) -> std::io::Result<std::process::Output> {
    crate::host_call::guard_program("gh", dir, args).map_err(std::io::Error::other)?;
    let mut cmd = Command::new("gh");
    if let Some(dir) = dir {
        cmd.current_dir(dir);
    }
    cmd.args(args).output()
}

/// `gh repo view --json nameWithOwner,defaultBranchRef,isFork,parent` over `origin`.
fn github_repo_info(repo_path: &Path) -> RepoInfo {
    let Ok(output) = gh(
        Some(repo_path),
        &["repo", "view", "--json", "nameWithOwner,defaultBranchRef,isFork,parent"],
    ) else {
        return RepoInfo::default();
    };
    if !output.status.success() {
        return RepoInfo::default();
    }
    let Ok(j) = serde_json::from_slice::<RepoViewJson>(&output.stdout) else {
        return RepoInfo::default();
    };
    let upstream = upstream_of(j.is_fork.unwrap_or(false), j.parent.as_ref());
    RepoInfo {
        repo: j.name_with_owner.clone(),
        github_repo: j.name_with_owner,
        default_branch: j.default_branch_ref.and_then(|b| b.name),
        upstream,
        ..RepoInfo::default()
    }
}

/// A plain remote's default branch: `git ls-remote --symref origin HEAD`'s `ref: refs/heads/<b>`.
fn plain_default_branch(repo_path: &Path) -> Option<String> {
    let output = git(repo_path)
        .args(["ls-remote", "--symref", "origin", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("ref: refs/heads/"))
        .and_then(|l| l.split('\t').next())
        .map(str::to_string)
}

/// Whether `origin` answers `git ls-remote origin` (the Setup check for a plain remote).
pub fn remote_reachable(repo_path: &Path) -> bool {
    git(repo_path)
        .args(["ls-remote", "origin"])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// A fork's parent and its default branch (`upstreamOf` in `setup.ts`).
fn upstream_of(is_fork: bool, parent: Option<&Parent>) -> Option<Upstream> {
    if !is_fork {
        return None;
    }
    let parent = parent?;
    let owner = parent.owner.as_ref()?.login.as_ref()?;
    let name = parent.name.as_ref()?;
    let repo = format!("{owner}/{name}");
    let default_branch = gh(None, &["repo", "view", &repo, "--json", "defaultBranchRef"])
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| serde_json::from_slice::<DefaultBranchOnly>(&o.stdout).ok())
        .and_then(|d| d.default_branch_ref)
        .and_then(|b| b.name);
    Some(Upstream { repo, default_branch })
}

/// `gh api user`'s `login`, or `None` when `gh` isn't signed in.
pub fn github_login() -> Option<String> {
    let output = gh(None, &["api", "user", "--jq", ".login"]).ok()?;
    if !output.status.success() {
        return None;
    }
    let login = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!login.is_empty()).then_some(login)
}

/// Whether `gh` is signed in (`gh auth status`'s exit code).
pub fn gh_authenticated() -> bool {
    gh(None, &["auth", "status"]).is_ok_and(|o| o.status.success())
}

/// `glab api user`'s `username`, or `None` when `glab` isn't signed in.
pub fn glab_login() -> Option<String> {
    crate::repo_gitlab::login("glab")
}

/// Whether `glab` is signed in (`glab auth status`'s exit code).
pub fn glab_authenticated() -> bool {
    crate::repo_gitlab::authenticated("glab")
}

/// The person's own login and their organisations (GitHub) or groups (GitLab), to list repos for
/// (`listOwners` in `github.ts`). The login is always first.
pub fn list_owners(host: CodeHost) -> Result<Vec<String>> {
    match host {
        CodeHost::Github => github_owners(),
        CodeHost::Gitlab => crate::repo_gitlab::list_owners("glab"),
        CodeHost::Plain => bail!("A plain git remote has no owners to list"),
    }
}

fn github_owners() -> Result<Vec<String>> {
    let login = github_login().context("gh isn't signed in")?;
    let output = gh(
        None,
        &[
            "api",
            "--paginate",
            "user/memberships/orgs?state=active",
            "--jq",
            ".[].organization.login",
        ],
    )
    .context("running gh api")?;
    let mut owners = vec![login];
    if output.status.success() {
        owners.extend(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
        );
    }
    Ok(owners)
}

/// One of `owner`'s repos (`listRepos` in `github.ts`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RepoSummary {
    #[serde(rename = "nameWithOwner")]
    pub name_with_owner: String,
    pub description: Option<String>,
    pub visibility: String,
    #[serde(rename = "isFork")]
    pub is_fork: bool,
}

/// `owner`'s repos, as `gh repo list` or `glab repo list` orders them.
pub fn list_repos(host: CodeHost, owner: &str) -> Result<Vec<RepoSummary>> {
    match host {
        CodeHost::Github => github_repos(owner),
        CodeHost::Gitlab => crate::repo_gitlab::list_repos("glab", owner),
        CodeHost::Plain => bail!("A plain git remote has no repos to list"),
    }
}

fn github_repos(owner: &str) -> Result<Vec<RepoSummary>> {
    let output = gh(
        None,
        &[
            "repo",
            "list",
            owner,
            "--limit",
            "1000",
            "--no-archived",
            "--json",
            "nameWithOwner,description,visibility,isFork",
        ],
    )
    .context("running gh repo list")?;
    if !output.status.success() {
        bail!(
            "Couldn't list {owner}'s repos: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    serde_json::from_slice(&output.stdout).context("parsing gh repo list")
}

static GITHUB_REMOTE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)github\.com[:/]([^/\s]+/[^/\s]+?)(?:\.git)?/?$").unwrap());

/// Where a project's `origin` lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeHost {
    Github,
    Gitlab,
    /// Any other host, a local path or `file://`: merged with plain git, no pull requests.
    Plain,
}

/// A remote URL with a scheme or a `user@` (HTTPS, `ssh://`, scp-style `git@host:path`):
/// capture 1 is the host, 2 the path.
static HOSTED_REMOTE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:[a-zA-Z][a-zA-Z0-9+.-]*://(?:[^@/\s]+@)?|[^@/\s]+@)([^/:\s]+)(?::\d+)?[:/](.+?)(?:\.git)?/?$")
        .unwrap()
});

fn gitlab_remote(url: &str) -> Option<(String, String)> {
    let c = HOSTED_REMOTE.captures(url.trim())?;
    c[1].to_lowercase()
        .contains("gitlab")
        .then(|| (c[1].to_string(), c[2].to_string()))
}

/// Which code host an `origin` URL is on: github.com, a GitLab host (gitlab.com or any host
/// containing `gitlab`, like `host_cli_of`), or anything else. Pure.
pub fn code_host(url: &str) -> CodeHost {
    if GITHUB_REMOTE.is_match(url.trim()) {
        CodeHost::Github
    } else if gitlab_remote(url).is_some() {
        CodeHost::Gitlab
    } else {
        CodeHost::Plain
    }
}

/// The project a remote URL points at: `owner/name` on GitHub, the full group path (nested
/// groups kept) on GitLab, or `None` for a plain remote. Pure (`remoteRepo` in `github.ts`).
pub fn remote_repo(url: &str) -> Option<String> {
    if let Some(c) = GITHUB_REMOTE.captures(url.trim()) {
        return Some(c[1].to_string());
    }
    gitlab_remote(url).map(|(_, path)| path)
}

pub(crate) fn git(repo_path: &Path) -> Command {
    crate::git_bin::command(repo_path)
}

/// `repo_path`'s `origin` remote, or `None` without one.
pub fn origin_url(repo_path: &Path) -> Option<String> {
    let output = git(repo_path).args(["remote", "get-url", "origin"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!url.is_empty()).then_some(url)
}

/// What identifies a remote: its project path on GitHub or GitLab, else the URL with a trailing
/// `/` and `.git` dropped (a plain remote).
fn remote_identity(url: &str) -> String {
    remote_repo(url).unwrap_or_else(|| {
        let u = url.trim().trim_end_matches('/');
        u.strip_suffix(".git").unwrap_or(u).to_string()
    })
}

/// A checkout of `github_repo` (or of a plain remote's URL) among `search_dir`'s immediate
/// subfolders: the first one whose `origin` remote matches, found by its `.git` folder (a narrowed `findCheckouts` in
/// `github.ts`, over one folder rather than every configured project folder).
pub fn find_checkout(search_dir: &Path, github_repo: &str) -> Option<PathBuf> {
    let wanted = remote_identity(github_repo);
    let entries = std::fs::read_dir(search_dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.join(".git").exists() {
            continue;
        }
        if origin_url(&path).as_deref().map(remote_identity).as_deref() == Some(wanted.as_str()) {
            return Some(path);
        }
    }
    None
}

/// Clones `repo` into `dest` with `gh repo clone` or `glab repo clone`.
pub fn clone_repo(host: CodeHost, repo: &str, dest: &Path) -> Result<()> {
    match host {
        CodeHost::Github => clone_github(repo, dest),
        CodeHost::Gitlab => crate::repo_gitlab::clone_repo("glab", repo, dest),
        CodeHost::Plain => bail!("A plain git remote can't be cloned by name"),
    }
}

static CLONE_LOCK: Mutex<()> = Mutex::new(());

/// Clones `github_repo` into `dest` unless a checkout of it is already in `search_dir`, holding
/// one process-wide lock throughout so a second clone of the same repo at the same moment waits
/// for the first, then finds its checkout instead of cloning again. Returns the checkout's path.
pub fn clone_or_find(search_dir: &Path, github_repo: &str, dest: &Path) -> Result<PathBuf> {
    clone_or_find_with(search_dir, github_repo, dest, || {
        clone_repo(CodeHost::Github, github_repo, dest)
    })
}

fn clone_or_find_with(
    search_dir: &Path,
    github_repo: &str,
    dest: &Path,
    clone: impl FnOnce() -> Result<()>,
) -> Result<PathBuf> {
    // A poisoned lock guards no data, so it is still safe to take.
    let _guard = CLONE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(found) = find_checkout(search_dir, github_repo) {
        return Ok(found);
    }
    clone()?;
    Ok(dest.to_path_buf())
}

fn clone_github(github_repo: &str, dest: &Path) -> Result<()> {
    let output = gh(None, &["repo", "clone", github_repo, &dest.to_string_lossy()]).context("running gh repo clone")?;
    if !output.status.success() {
        bail!(
            "Couldn't clone {github_repo}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

/// Clones a plain remote's `url` into `dest` with `git clone`.
pub fn clone_remote(url: &str, dest: &Path) -> Result<()> {
    let cwd = dest.parent().filter(|p| p.is_dir()).unwrap_or(Path::new("."));
    let output = git(cwd)
        .args(["clone", url])
        .arg(dest)
        .output()
        .context("running git clone")?;
    if !output.status.success() {
        bail!(
            "Couldn't clone {url}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

/// The base branch Setup offers a developer on a team (github-workflow.md section 4.2).
pub fn dev_branch(login: &str) -> String {
    format!("dev/{login}")
}

/// Creates `branch` on `origin` from `origin/<from>` when `origin` doesn't have it yet
/// (`createBranchIfMissing` in `setup.ts`). `Ok(true)` when it created the branch, `Ok(false)`
/// when `origin` already had it.
pub fn create_branch_if_missing(repo_path: &Path, branch: &str, from: &str) -> Result<bool> {
    let ls = git(repo_path)
        .args([
            "ls-remote",
            "--exit-code",
            "--heads",
            "origin",
            &format!("refs/heads/{branch}"),
        ])
        .output()
        .context("running git ls-remote")?;
    if ls.status.success() {
        return Ok(false);
    }
    if ls.status.code() != Some(2) {
        bail!(
            "Couldn't check origin for {branch}: {}",
            String::from_utf8_lossy(&ls.stderr).trim()
        );
    }
    let fetch = git(repo_path)
        .args(["fetch", "origin", from])
        .output()
        .context("running git fetch")?;
    if !fetch.status.success() {
        bail!(
            "Couldn't fetch origin/{from}: {}",
            String::from_utf8_lossy(&fetch.stderr).trim()
        );
    }
    let push = git(repo_path)
        .args([
            "push",
            "origin",
            &format!("refs/remotes/origin/{from}:refs/heads/{branch}"),
        ])
        .output()
        .context("running git push")?;
    if !push.status.success() {
        bail!(
            "Couldn't create {branch} from {from}: {}",
            String::from_utf8_lossy(&push.stderr).trim()
        );
    }
    Ok(true)
}

/// Whether `backlog_path` (repo-relative, `/`-separated as git itself uses) is in the tree of
/// `origin/<base_branch>`: the "backlog file that isn't on origin/<baseBranch>" problem in
/// docs/specs/agent/09-web-ui.md section 9.1.
pub fn backlog_on_origin(repo_path: &Path, base_branch: &str, backlog_path: &str) -> Result<bool> {
    let output = git(repo_path)
        .args(["cat-file", "-e", &format!("origin/{base_branch}:{backlog_path}")])
        .output()
        .context("running git cat-file")?;
    Ok(output.status.success())
}

/// What's already known about a checkout, to turn into "why a folder won't do" (section 9.1).
#[derive(Debug, Clone, Default)]
pub struct RepoCheck {
    pub is_git_repo: bool,
    pub github_repo: Option<String>,
    /// The host's CLI (`gh` on GitHub, `glab` on GitLab) is signed in. Ignored for a plain remote.
    pub host_cli_authenticated: bool,
    /// `None` when the checkout has no `origin`.
    pub code_host: Option<CodeHost>,
    /// The error text from `create_branch_if_missing`, when the GitHub workflow pattern asked
    /// for an integration branch and creating it failed. `None` when none was needed, or it
    /// succeeded.
    pub integration_branch_error: Option<String>,
    /// `Some(false)` when the backlog file isn't on `origin/<baseBranch>` yet. `None` when
    /// there's no base branch or backlog path to check yet.
    pub backlog_on_origin: Option<bool>,
    /// A git repo with no remote: no CLI, remote or integration branch is needed.
    pub local_only: bool,
    /// `Some(false)` when a local-only repo has no commit on its base branch; `None` when not
    /// checked.
    pub base_branch_exists: Option<bool>,
}

/// "The repo" bullet's own wording (section 9.1) for why a folder won't do, over an already
/// gathered `RepoCheck`. Empty means there's nothing wrong.
pub fn repo_problems(check: &RepoCheck, base_branch: &str) -> Vec<String> {
    if !check.is_git_repo {
        return vec!["The repo path isn't a git repo.".to_string()];
    }
    if check.local_only {
        if check.base_branch_exists == Some(false) {
            return vec![format!(
                "There's no {base_branch} branch in this repo yet. Commit something to it first."
            )];
        }
        return Vec::new();
    }
    let mut problems = Vec::new();
    // A plain remote is accepted: work merges with plain git and there are no pull requests,
    // so it needs no CLI and isn't a problem.
    match check.code_host {
        Some(CodeHost::Plain) => {}
        Some(CodeHost::Gitlab) => {
            if !check.host_cli_authenticated {
                problems.push("glab isn't authenticated. Run `glab auth login`.".to_string());
            }
        }
        _ => {
            if !check.host_cli_authenticated {
                problems.push("gh isn't authenticated. Run `gh auth login`.".to_string());
            }
            if check.github_repo.is_none() {
                problems.push("No GitHub remote found for this repo.".to_string());
            }
        }
    }
    if let Some(err) = &check.integration_branch_error {
        problems.push(err.clone());
    }
    if check.backlog_on_origin == Some(false) {
        problems.push(format!("The backlog file isn't on origin/{base_branch}."));
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{git, repo_with_origin, run};
    use std::sync::Barrier;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn remote_repo_reads_an_https_url() {
        assert_eq!(
            remote_repo("https://github.com/Retaskable/retaskable-agent.git"),
            Some("Retaskable/retaskable-agent".to_string())
        );
    }

    #[test]
    fn remote_repo_reads_an_ssh_url() {
        assert_eq!(
            remote_repo("git@github.com:Retaskable/retaskable-agent.git"),
            Some("Retaskable/retaskable-agent".to_string())
        );
    }

    #[test]
    fn remote_repo_is_none_for_a_plain_remote() {
        assert_eq!(remote_repo("https://example.com/Retaskable/retaskable-agent.git"), None);
        assert_eq!(remote_repo("/local/path/repo.git"), None);
    }

    #[test]
    fn a_plain_remote_is_handled_with_git_alone() {
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_origin(root.path());
        let info = repo_info(&repo);
        assert_eq!(info.github_repo, None);
        assert_eq!(info.default_branch.as_deref(), Some("main"));
        assert!(remote_reachable(&repo));
        let gone = tempfile::tempdir().unwrap();
        let broken = repo_with_origin(gone.path());
        git(&broken)
            .args(["remote", "set-url", "origin"])
            .arg(gone.path().join("missing.git"))
            .output()
            .unwrap();
        assert!(!remote_reachable(&broken));
    }

    #[test]
    fn clone_remote_makes_a_findable_checkout_with_no_problems() {
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_origin(root.path());
        let url = origin_url(&repo).unwrap();
        let search = root.path().join("projects");
        std::fs::create_dir(&search).unwrap();
        let dest = search.join("clone");
        clone_remote(&url, &dest).unwrap();
        assert_eq!(origin_url(&dest).as_deref(), Some(url.as_str()));
        assert_eq!(find_checkout(&search, &url), Some(dest.clone()));
        assert_eq!(find_checkout(&search, &format!("{url}/")), Some(dest.clone()));
        let check = RepoCheck {
            is_git_repo: true,
            github_repo: repo_info(&dest).github_repo,
            code_host: Some(code_host(&url)),
            ..RepoCheck::default()
        };
        assert!(repo_problems(&check, "main").is_empty());
        assert!(backlog_on_origin(&dest, "main", "README.md").unwrap());
        assert!(create_branch_if_missing(&dest, "dev/x", "main").unwrap());
        assert!(clone_remote(&url, &dest).is_err());
    }

    #[test]
    fn dev_branch_is_prefixed() {
        assert_eq!(dev_branch("octocat"), "dev/octocat");
    }

    #[test]
    fn create_branch_if_missing_creates_a_missing_branch() {
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_origin(root.path());
        let created = create_branch_if_missing(&repo, "dev/octocat", "main").unwrap();
        assert!(created);
        let ls = git(&repo)
            .args([
                "ls-remote",
                "--exit-code",
                "--heads",
                "origin",
                "refs/heads/dev/octocat",
            ])
            .output()
            .unwrap();
        assert!(ls.status.success());
    }

    #[test]
    fn create_branch_if_missing_leaves_an_existing_branch() {
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_origin(root.path());
        assert!(create_branch_if_missing(&repo, "dev/octocat", "main").unwrap());
        assert!(!create_branch_if_missing(&repo, "dev/octocat", "main").unwrap());
    }

    #[test]
    fn backlog_on_origin_is_true_for_a_committed_file() {
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_origin(root.path());
        assert!(backlog_on_origin(&repo, "main", "README.md").unwrap());
    }

    #[test]
    fn backlog_on_origin_is_false_for_a_missing_file() {
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_origin(root.path());
        assert!(!backlog_on_origin(&repo, "main", "docs/BACKLOG.md").unwrap());
    }

    #[test]
    fn backlog_on_origin_is_false_for_a_file_only_on_the_local_branch() {
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_origin(root.path());
        std::fs::write(repo.join("BACKLOG.md"), "todo").unwrap();
        run(git(&repo).args(["add", "BACKLOG.md"]));
        run(git(&repo).args(["commit", "--quiet", "-m", "add backlog"]));
        assert!(!backlog_on_origin(&repo, "main", "BACKLOG.md").unwrap());
    }

    /// A fake clone: makes a repo in `dest` whose origin is `url`, counting its calls.
    fn fake_clone(search: &Path, url: &str, calls: &AtomicUsize) -> Result<()> {
        calls.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(std::time::Duration::from_millis(100));
        let repo = repo_with_origin(search);
        run(git(&repo).args(["remote", "set-url", "origin", url]));
        Ok(())
    }

    #[test]
    fn clone_or_find_clones_once_for_two_simultaneous_calls() {
        let root = tempfile::tempdir().unwrap();
        let search = root.path();
        let dest = search.join("repo");
        let calls = AtomicUsize::new(0);
        let barrier = Barrier::new(2);
        let url = "https://github.com/octocat/repo.git";
        let results: Vec<PathBuf> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..2)
                .map(|_| {
                    s.spawn(|| {
                        barrier.wait();
                        clone_or_find_with(search, "octocat/repo", &dest, || fake_clone(search, url, &calls)).unwrap()
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(results, vec![dest.clone(), dest]);
    }

    #[test]
    fn clone_or_find_returns_an_existing_checkout_without_cloning() {
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_origin(root.path());
        run(git(&repo).args(["remote", "set-url", "origin", "https://github.com/octocat/repo.git"]));
        let calls = AtomicUsize::new(0);
        let dest = root.path().join("other");
        let found = clone_or_find_with(root.path(), "octocat/repo", &dest, || {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .unwrap();
        assert_eq!(found, repo);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn clone_or_find_returns_a_clone_error_and_leaves_the_lock_free() {
        let root = tempfile::tempdir().unwrap();
        let dest = root.path().join("repo");
        let err = clone_or_find_with(root.path(), "octocat/repo", &dest, || bail!("no network")).unwrap_err();
        assert!(err.to_string().contains("no network"));
        let calls = AtomicUsize::new(0);
        let path = clone_or_find_with(root.path(), "octocat/repo", &dest, || {
            fake_clone(root.path(), "https://github.com/octocat/repo.git", &calls)
        })
        .unwrap();
        assert_eq!(path, dest);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn find_checkout_matches_a_sibling_by_origin() {
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_origin(root.path());
        // `git remote set-url` doesn't fetch or validate, so a made-up GitHub URL is fine here.
        run(git(&repo).args(["remote", "set-url", "origin", "https://github.com/octocat/repo.git"]));
        assert_eq!(find_checkout(root.path(), "octocat/repo"), Some(repo));
    }

    #[test]
    fn find_checkout_is_none_without_a_match() {
        let root = tempfile::tempdir().unwrap();
        repo_with_origin(root.path());
        assert_eq!(find_checkout(root.path(), "octocat/repo"), None);
    }

    #[test]
    fn code_host_tells_the_hosts_apart() {
        for (url, host) in [
            ("https://github.com/o/r.git", CodeHost::Github),
            ("git@github.com:o/r.git", CodeHost::Github),
            ("https://gitlab.com/g/r.git", CodeHost::Gitlab),
            ("git@gitlab.com:g/r.git", CodeHost::Gitlab),
            ("https://gitlab.example.org/g/r", CodeHost::Gitlab),
            ("ssh://git@code.gitlab.corp:2222/g/r.git", CodeHost::Gitlab),
            ("ssh://example.com/x.git", CodeHost::Plain),
            ("/local/path/repo.git", CodeHost::Plain),
            ("gitlab-stuff/foo/bar", CodeHost::Plain),
            ("file:///srv/gitlab/repo.git", CodeHost::Plain),
        ] {
            assert_eq!(code_host(url), host, "{url}");
        }
    }

    #[test]
    fn remote_repo_reads_gitlab_urls() {
        assert_eq!(
            remote_repo("https://gitlab.com/group/name.git").as_deref(),
            Some("group/name")
        );
        assert_eq!(
            remote_repo("git@gitlab.com:group/name.git").as_deref(),
            Some("group/name")
        );
        assert_eq!(
            remote_repo("https://gitlab.com/group/sub/name.git/").as_deref(),
            Some("group/sub/name")
        );
        assert_eq!(
            remote_repo("ssh://git@gitlab.example.org:2222/a/b/c").as_deref(),
            Some("a/b/c")
        );
        assert_eq!(remote_repo("ssh://example.com/x.git"), None);
    }

    #[test]
    fn find_checkout_matches_a_gitlab_sibling() {
        let root = tempfile::tempdir().unwrap();
        let repo = repo_with_origin(root.path());
        let _ = std::fs::remove_dir_all(&repo);
        let gl = root.path().join("gl");
        std::fs::create_dir_all(&gl).unwrap();
        let run = |args: &[&str]| {
            assert!(git(&gl).args(args).status().unwrap().success());
        };
        run(&["init", "-q"]);
        run(&["remote", "add", "origin", "https://gitlab.com/group/sub/name.git"]);
        assert_eq!(find_checkout(root.path(), "group/sub/name"), Some(gl));
    }

    #[test]
    fn repo_problems_on_gitlab_and_plain() {
        let mut check = RepoCheck {
            is_git_repo: true,
            github_repo: Some("g/r".to_string()),
            code_host: Some(CodeHost::Gitlab),
            ..Default::default()
        };
        assert_eq!(
            repo_problems(&check, "main"),
            vec!["glab isn't authenticated. Run `glab auth login`."]
        );
        check.host_cli_authenticated = true;
        assert!(repo_problems(&check, "main").is_empty());
        check = RepoCheck {
            is_git_repo: true,
            code_host: Some(CodeHost::Plain),
            ..Default::default()
        };
        assert!(repo_problems(&check, "main").is_empty());
    }

    #[test]
    fn repo_problems_reports_not_a_git_repo_alone() {
        let check = RepoCheck {
            is_git_repo: false,
            ..Default::default()
        };
        assert_eq!(repo_problems(&check, "main"), vec!["The repo path isn't a git repo."]);
    }

    #[test]
    fn repo_problems_is_empty_when_everything_is_fine() {
        let check = RepoCheck {
            is_git_repo: true,
            github_repo: Some("octocat/repo".to_string()),
            host_cli_authenticated: true,
            code_host: Some(CodeHost::Github),
            integration_branch_error: None,
            backlog_on_origin: Some(true),
            ..Default::default()
        };
        assert!(repo_problems(&check, "main").is_empty());
    }

    #[test]
    fn repo_problems_reports_every_other_problem_together() {
        let check = RepoCheck {
            is_git_repo: true,
            github_repo: None,
            host_cli_authenticated: false,
            code_host: None,
            integration_branch_error: Some("Couldn't create dev/octocat from main: no such ref".to_string()),
            backlog_on_origin: Some(false),
            ..Default::default()
        };
        assert_eq!(
            repo_problems(&check, "main"),
            vec![
                "gh isn't authenticated. Run `gh auth login`.",
                "No GitHub remote found for this repo.",
                "Couldn't create dev/octocat from main: no such ref",
                "The backlog file isn't on origin/main.",
            ]
        );
    }
}
