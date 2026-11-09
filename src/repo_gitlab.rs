//! The GitLab half of `repo` (docs/specs/agent/09.1-what-the-computer-works-out-for-a-projects-settings.md):
//! the same questions `repo` asks `gh`, asked of `glab`. Every function takes the program to run,
//! so tests can point it at a fake script; `repo` passes `"glab"`.
use std::path::Path;
use std::process::{Command, Output};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::repo::{RepoInfo, RepoSummary, Upstream};

fn run(program: &str, dir: Option<&Path>, args: &[&str]) -> std::io::Result<Output> {
    crate::host_call::guard_program(program, dir, args).map_err(std::io::Error::other)?;
    let mut cmd = Command::new(program);
    if let Some(dir) = dir {
        cmd.current_dir(dir);
    }
    cmd.args(args).output()
}

fn stdout_of(program: &str, dir: Option<&Path>, args: &[&str]) -> Option<Vec<u8>> {
    run(program, dir, args)
        .ok()
        .filter(|o| o.status.success())
        .map(|o| o.stdout)
}

/// A GitLab project as `glab repo view`/`glab repo list` print it with `--output json`.
#[derive(Deserialize)]
struct Project {
    path_with_namespace: Option<String>,
    default_branch: Option<String>,
    description: Option<String>,
    visibility: Option<String>,
    forked_from_project: Option<Parent>,
}

#[derive(Deserialize)]
struct Parent {
    path_with_namespace: Option<String>,
}

/// The checkout's project path, default branch and (for a fork) upstream, from
/// `glab repo view --output json`. Empty when `glab` can't answer.
pub fn repo_info(program: &str, repo_path: &Path) -> RepoInfo {
    let Some(project) = stdout_of(program, Some(repo_path), &["repo", "view", "--output", "json"])
        .and_then(|out| serde_json::from_slice::<Project>(&out).ok())
    else {
        return RepoInfo::default();
    };
    let upstream = project
        .forked_from_project
        .and_then(|p| p.path_with_namespace)
        .map(|repo| {
            let default_branch = stdout_of(program, None, &["repo", "view", &repo, "--output", "json"])
                .and_then(|out| serde_json::from_slice::<Project>(&out).ok())
                .and_then(|p| p.default_branch);
            Upstream { repo, default_branch }
        });
    RepoInfo {
        repo: project.path_with_namespace,
        default_branch: project.default_branch,
        upstream,
        ..RepoInfo::default()
    }
}

/// `glab api user`'s `username`, or `None` when `glab` isn't signed in.
pub fn login(program: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct User {
        username: Option<String>,
    }
    let out = stdout_of(program, None, &["api", "user"])?;
    serde_json::from_slice::<User>(&out)
        .ok()?
        .username
        .filter(|u| !u.is_empty())
}

/// Whether `glab` is signed in (`glab auth status`'s exit code).
pub fn authenticated(program: &str) -> bool {
    run(program, None, &["auth", "status"]).is_ok_and(|o| o.status.success())
}

/// The person's own login, then their groups' full paths.
pub fn list_owners(program: &str) -> Result<Vec<String>> {
    #[derive(Deserialize)]
    struct Group {
        full_path: String,
    }
    let mut owners = vec![login(program).context("glab isn't signed in")?];
    if let Some(out) = stdout_of(program, None, &["api", "groups?min_access_level=10", "--paginate"]) {
        // `--paginate` prints one JSON array per page, back to back.
        for page in serde_json::Deserializer::from_slice(&out).into_iter::<Vec<Group>>() {
            let Ok(groups) = page else { break };
            owners.extend(groups.into_iter().map(|g| g.full_path));
        }
    }
    Ok(owners)
}

/// `owner`'s repos from `glab repo list`: the person's own, or a group's with `--group`.
pub fn list_repos(program: &str, owner: &str) -> Result<Vec<RepoSummary>> {
    let mut args = vec!["repo", "list", "--per-page", "100", "--output", "json"];
    if login(program).as_deref() == Some(owner) {
        args.push("--mine");
    } else {
        args.extend(["--group", owner]);
    }
    let output = run(program, None, &args).context("running glab repo list")?;
    if !output.status.success() {
        bail!(
            "Couldn't list {owner}'s repos: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let projects: Vec<Project> = serde_json::from_slice(&output.stdout).context("parsing glab repo list")?;
    Ok(projects
        .into_iter()
        .filter_map(|p| {
            Some(RepoSummary {
                name_with_owner: p.path_with_namespace?,
                description: p.description.filter(|d| !d.is_empty()),
                visibility: p.visibility.unwrap_or_default(),
                is_fork: p.forked_from_project.is_some(),
            })
        })
        .collect())
}

/// Clones `repo` into `dest` with `glab repo clone`.
pub fn clone_repo(program: &str, repo: &str, dest: &Path) -> Result<()> {
    let args = ["repo", "clone", repo];
    crate::host_call::guard_program(program, None, &args)?;
    let output = Command::new(program)
        .args(args)
        .arg(dest)
        .output()
        .context("running glab repo clone")?;
    if !output.status.success() {
        bail!(
            "Couldn't clone {repo}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::test_support::write_executable;

    fn fake(dir: &Path, body: &str) -> String {
        let p = dir.join("glab");
        write_executable(&p, &format!("#!/bin/sh\n{body}\n"));
        p.to_string_lossy().into_owned()
    }

    const FAKE: &str = r#"
echo "$@" >> "$(dirname "$0")/calls"
case "$*" in
"repo view --output json") echo '{"path_with_namespace":"grp/sub/app","default_branch":"trunk","forked_from_project":{"path_with_namespace":"up/app"}}';;
"repo view up/app --output json") echo '{"path_with_namespace":"up/app","default_branch":"main"}';;
"api user") echo '{"username":"jc"}';;
"api groups?min_access_level=10 --paginate") echo '[{"full_path":"grp"}][{"full_path":"grp/sub"}]';;
"repo list --per-page 100 --output json --group grp") echo '[{"path_with_namespace":"grp/a","description":"","visibility":"private","forked_from_project":{"id":1}},{"path_with_namespace":"grp/b","description":"B","visibility":"public"}]';;
"repo clone grp/a "*) ;;
*) echo "no $*" >&2; exit 1;;
esac"#;

    #[test]
    fn repo_info_reads_project_and_fork_upstream() {
        let dir = tempfile::tempdir().unwrap();
        let glab = fake(dir.path(), FAKE);
        let info = repo_info(&glab, dir.path());
        assert_eq!(info.repo.as_deref(), Some("grp/sub/app"));
        assert_eq!(info.default_branch.as_deref(), Some("trunk"));
        assert_eq!(
            info.upstream,
            Some(Upstream {
                repo: "up/app".into(),
                default_branch: Some("main".into())
            })
        );
    }

    #[test]
    fn repo_info_is_empty_when_glab_fails() {
        let dir = tempfile::tempdir().unwrap();
        let glab = fake(dir.path(), "exit 1");
        assert_eq!(repo_info(&glab, dir.path()), RepoInfo::default());
    }

    #[test]
    fn owners_are_the_login_then_groups() {
        let dir = tempfile::tempdir().unwrap();
        let glab = fake(dir.path(), FAKE);
        assert_eq!(list_owners(&glab).unwrap(), ["jc", "grp", "grp/sub"]);
    }

    #[test]
    fn list_repos_parses_glab_output() {
        let dir = tempfile::tempdir().unwrap();
        let glab = fake(dir.path(), FAKE);
        let repos = list_repos(&glab, "grp").unwrap();
        assert_eq!(repos.len(), 2);
        assert_eq!(repos[0].name_with_owner, "grp/a");
        assert_eq!(repos[0].description, None);
        assert!(repos[0].is_fork);
        assert_eq!(repos[1].description.as_deref(), Some("B"));
        assert_eq!(repos[1].visibility, "public");
        assert!(!repos[1].is_fork);
    }

    #[test]
    fn clone_passes_the_destination_and_reports_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let glab = fake(dir.path(), FAKE);
        let dest = dir.path().join("out");
        clone_repo(&glab, "grp/a", &dest).unwrap();
        let calls = std::fs::read_to_string(dir.path().join("calls")).unwrap();
        assert!(calls.contains(&format!("repo clone grp/a {}", dest.display())));
        let err = clone_repo(&glab, "grp/zzz", &dest).unwrap_err().to_string();
        assert!(err.contains("no repo clone grp/zzz"), "{err}");
    }
}
