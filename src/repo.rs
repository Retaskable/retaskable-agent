//! "The repo" (docs/specs/agent/09-web-ui.md section 9.1), ported from `repoInfo`, `githubLogin`,
//! `devBranch`, `createBranchIfMissing` (`agent/src/setup.ts`) and `listOwners`/`listRepos`/
//! `remoteRepo` (`agent/src/github.ts`). Only the plain-GitHub-repo path: a local folder, or one
//! of the person's (and their GitHub organisations') `gh repo list` repos. GitLab, a plain git
//! remote, a local-only project and the full `GithubPicker`'s caching and clone-serialising lock
//! aren't ported, since this agent's trimmed spec doesn't reach them yet.
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;

use anyhow::{Context, Result, bail};
use regex::Regex;
use serde::Deserialize;

/// `gh repo view`'s answer about a checkout's GitHub identity (`repoInfo` in `setup.ts`). Every
/// field is `None` when `gh` can't answer: no GitHub remote, `gh` not authenticated, or no repo
/// there at all.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoInfo {
    pub github_repo: Option<String>,
    pub default_branch: Option<String>,
    pub upstream: Option<Upstream>,
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

/// The checkout's `githubRepo`, default branch and (for a fork) upstream, read from `origin` with
/// `gh repo view --json nameWithOwner,defaultBranchRef,isFork,parent`.
pub fn repo_info(repo_path: &Path) -> RepoInfo {
    let Ok(output) = Command::new("gh")
        .current_dir(repo_path)
        .args(["repo", "view", "--json", "nameWithOwner,defaultBranchRef,isFork,parent"])
        .output()
    else {
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
        github_repo: j.name_with_owner,
        default_branch: j.default_branch_ref.and_then(|b| b.name),
        upstream,
    }
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
    let default_branch = Command::new("gh")
        .args(["repo", "view", &repo, "--json", "defaultBranchRef"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| serde_json::from_slice::<DefaultBranchOnly>(&o.stdout).ok())
        .and_then(|d| d.default_branch_ref)
        .and_then(|b| b.name);
    Some(Upstream { repo, default_branch })
}

/// `gh api user`'s `login`, or `None` when `gh` isn't signed in.
pub fn github_login() -> Option<String> {
    let output = Command::new("gh")
        .args(["api", "user", "--jq", ".login"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let login = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!login.is_empty()).then_some(login)
}

/// Whether `gh` is signed in (`gh auth status`'s exit code).
pub fn gh_authenticated() -> bool {
    Command::new("gh")
        .args(["auth", "status"])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// The person's own login and GitHub organisations, to list repos for (`listOwners` in
/// `github.ts`). The login is always first.
pub fn list_owners() -> Result<Vec<String>> {
    let login = github_login().context("gh isn't signed in")?;
    let output = Command::new("gh")
        .args([
            "api",
            "--paginate",
            "user/memberships/orgs?state=active",
            "--jq",
            ".[].organization.login",
        ])
        .output()
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

/// `owner`'s repos, as `gh repo list` orders them.
pub fn list_repos(owner: &str) -> Result<Vec<RepoSummary>> {
    let output = Command::new("gh")
        .args([
            "repo",
            "list",
            owner,
            "--limit",
            "1000",
            "--no-archived",
            "--json",
            "nameWithOwner,description,visibility,isFork",
        ])
        .output()
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

/// The `owner/name` a remote URL points at on GitHub, or `None` for anything else (a GitLab
/// host, a plain path, `file://`). Pure (`remoteRepo` in `github.ts`).
pub fn remote_repo(url: &str) -> Option<String> {
    GITHUB_REMOTE.captures(url.trim()).map(|c| c[1].to_string())
}

fn git(repo_path: &Path) -> Command {
    crate::git_bin::command(repo_path)
}

/// `repo_path`'s `origin` remote, or `None` without one.
fn origin_url(repo_path: &Path) -> Option<String> {
    let output = git(repo_path).args(["remote", "get-url", "origin"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!url.is_empty()).then_some(url)
}

/// A checkout of `github_repo` among `search_dir`'s immediate subfolders: the first one whose
/// `origin` remote matches, found by its `.git` folder (a narrowed `findCheckouts` in
/// `github.ts`, over one folder rather than every configured project folder).
pub fn find_checkout(search_dir: &Path, github_repo: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(search_dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.join(".git").exists() {
            continue;
        }
        if origin_url(&path).as_deref().and_then(remote_repo).as_deref() == Some(github_repo) {
            return Some(path);
        }
    }
    None
}

/// Clones `github_repo` into `dest` with `gh repo clone`.
pub fn clone_repo(github_repo: &str, dest: &Path) -> Result<()> {
    let output = Command::new("gh")
        .args(["repo", "clone", github_repo])
        .arg(dest)
        .output()
        .context("running gh repo clone")?;
    if !output.status.success() {
        bail!(
            "Couldn't clone {github_repo}: {}",
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
    pub gh_authenticated: bool,
    /// The error text from `create_branch_if_missing`, when the GitHub workflow pattern asked
    /// for an integration branch and creating it failed. `None` when none was needed, or it
    /// succeeded.
    pub integration_branch_error: Option<String>,
    /// `Some(false)` when the backlog file isn't on `origin/<baseBranch>` yet. `None` when
    /// there's no base branch or backlog path to check yet.
    pub backlog_on_origin: Option<bool>,
}

/// "The repo" bullet's own wording (section 9.1) for why a folder won't do, over an already
/// gathered `RepoCheck`. Empty means there's nothing wrong.
pub fn repo_problems(check: &RepoCheck, base_branch: &str) -> Vec<String> {
    if !check.is_git_repo {
        return vec!["The repo path isn't a git repo.".to_string()];
    }
    let mut problems = Vec::new();
    if !check.gh_authenticated {
        problems.push("gh isn't authenticated. Run `gh auth login`.".to_string());
    }
    if check.github_repo.is_none() {
        problems.push("No GitHub remote found for this repo.".to_string());
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
    fn remote_repo_is_none_for_a_non_github_host() {
        assert_eq!(remote_repo("https://gitlab.com/Retaskable/retaskable-agent.git"), None);
        assert_eq!(remote_repo("/local/path/repo.git"), None);
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
            gh_authenticated: true,
            integration_branch_error: None,
            backlog_on_origin: Some(true),
        };
        assert!(repo_problems(&check, "main").is_empty());
    }

    #[test]
    fn repo_problems_reports_every_other_problem_together() {
        let check = RepoCheck {
            is_git_repo: true,
            github_repo: None,
            gh_authenticated: false,
            integration_branch_error: Some("Couldn't create dev/octocat from main: no such ref".to_string()),
            backlog_on_origin: Some(false),
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
