//! GitHub's numeric repo id, which every program names a project's backlog by.
use std::path::Path;
use std::time::Duration;

/// How long `gh` may take to answer; a hung `gh` is a failed try, not a stuck loop.
const ASK: Duration = Duration::from_secs(30);

/// Why the id couldn't be learned, in words, and how long to wait before asking again when GitHub said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unknown {
    pub why: String,
    /// Until GitHub's limit resets (and a few seconds), when it was the limit.
    pub retry_after: Option<Duration>,
}

/// GitHub's numeric repo id (`gh api repos/<repo> --jq .id`, `repoId` in `agent/src/gh.ts`), which names the
/// project's backlog in the store.
pub async fn repo_id(github_repo: &str) -> Option<u64> {
    lookup(github_repo).await.ok()
}

/// [`repo_id`], or why not: a used-up GitHub API limit is said as that (with when it resets), not as a failure.
pub async fn lookup(github_repo: &str) -> Result<u64, Unknown> {
    lookup_with(Path::new("gh"), github_repo).await
}

pub(crate) async fn lookup_with(gh: &Path, github_repo: &str) -> Result<u64, Unknown> {
    let other = || Unknown {
        why: format!("gh couldn't say the id of {github_repo}"),
        retry_after: None,
    };
    let path = format!("repos/{github_repo}");
    let args = ["api", path.as_str(), "--jq", ".id"];
    crate::host_call::guard_program("gh", None, &args).map_err(|_| other())?;
    let run = crate::spawn::tokio_command(gh)
        .args(args)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(ASK, run)
        .await
        .map_err(|_| other())?
        .map_err(|_| other())?;
    if !out.status.success() {
        let said = format!(
            "{}\n{}",
            String::from_utf8_lossy(&out.stderr),
            String::from_utf8_lossy(&out.stdout)
        );
        if crate::gh_limit::is_limited(&said) {
            let now = crate::gh_limit::now();
            let reset = match crate::gh_limit::reset_in(&said, now) {
                Some(r) => Some(r),
                None => crate::gh_limit::ask_reset(gh).await,
            };
            return Err(Unknown {
                why: crate::gh_limit::message(reset),
                retry_after: crate::gh_limit::until(reset, now),
            });
        }
        return Err(other());
    }
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .ok()
        .filter(|&n| n > 0)
        .ok_or_else(other)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake(dir: &Path, body: &str) -> std::path::PathBuf {
        let p = dir.join("gh");
        crate::test_support::write_executable(&p, &format!("#!/bin/sh\n{body}\n"));
        p
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn says_the_id_and_the_limit_apart_from_other_failures() {
        let dir = tempfile::tempdir().unwrap();
        let ok = fake(dir.path(), "echo 12345");
        assert_eq!(lookup_with(&ok, "o/r").await, Ok(12345));

        let reset = crate::gh_limit::now() + 600;
        let limited = fake(
            dir.path(),
            &format!(
                "case \"$2\" in rate_limit) echo {reset};; *) echo 'gh: API rate limit exceeded for user ID 7. (HTTP 403)' >&2; exit 1;; esac"
            ),
        );
        let e = lookup_with(&limited, "o/r").await.unwrap_err();
        assert!(
            e.why
                .starts_with("GitHub's API limit for this account is used up; it resets at "),
            "{}",
            e.why
        );
        assert!(!e.why.contains("signed in"));
        let wait = e.retry_after.unwrap();
        assert!(wait > Duration::from_secs(590) && wait <= Duration::from_secs(606));

        let broken = fake(dir.path(), "echo 'HTTP 404: Not Found' >&2; exit 1");
        let e = lookup_with(&broken, "o/r").await.unwrap_err();
        assert_eq!(e.why, "gh couldn't say the id of o/r");
        assert_eq!(e.retry_after, None);
    }
}
