//! docs/specs/agent-ci.md section 3.1: the repo's own bare mirror. `retaskable-ci` has no project checkout, so each
//! repo gets a bare git directory under its data folder whose `origin` is the repo's URL. Each poll fetches the base
//! branch into `refs/remotes/origin/<base>`, which discovery reads with `git show` and a check's fetch step clones
//! from. The fetch uses the host's own git credentials, as the check's fetch does.
use std::ffi::OsStr;
use std::path::Path;

use super::discovery::git;

/// Makes the mirror when it isn't there yet, and points its `origin` at `url`.
pub async fn ensure(program: &OsStr, mirror: &Path, url: &str) -> Result<(), String> {
    if !mirror.join("HEAD").exists() {
        std::fs::create_dir_all(mirror).map_err(|e| format!("creating {}: {e}", mirror.display()))?;
        git(program, mirror, &["init", "--quiet", "--bare"]).await?;
    }
    if git(program, mirror, &["remote", "get-url", "origin"])
        .await
        .as_deref()
        .map(str::trim)
        == Ok(url)
    {
        return Ok(());
    }
    // A remote that exists with another URL is set; one that doesn't is added.
    if git(program, mirror, &["remote", "set-url", "origin", url])
        .await
        .is_err()
    {
        git(program, mirror, &["remote", "add", "origin", url]).await?;
    }
    Ok(())
}

/// Fetches `origin`'s `base` into the mirror's `refs/remotes/origin/<base>`.
pub async fn refresh(program: &OsStr, mirror: &Path, base: &str) -> Result<(), String> {
    let spec = format!("+refs/heads/{base}:refs/remotes/origin/{base}");
    git(program, mirror, &["fetch", "--quiet", "--no-tags", "origin", &spec]).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_mirror_is_made_once_and_follows_a_changed_url() {
        let d = tempfile::tempdir().unwrap();
        let mirror = d.path().join("m.git");
        let git_program = OsStr::new("git");
        ensure(git_program, &mirror, "https://example.invalid/a.git")
            .await
            .unwrap();
        ensure(git_program, &mirror, "https://example.invalid/a.git")
            .await
            .unwrap();
        let url = git(git_program, &mirror, &["remote", "get-url", "origin"])
            .await
            .unwrap();
        assert_eq!(url.trim(), "https://example.invalid/a.git");
        ensure(git_program, &mirror, "https://example.invalid/b.git")
            .await
            .unwrap();
        let url = git(git_program, &mirror, &["remote", "get-url", "origin"])
            .await
            .unwrap();
        assert_eq!(url.trim(), "https://example.invalid/b.git");
        assert!(refresh(git_program, &mirror, "main").await.is_err());
    }
}
