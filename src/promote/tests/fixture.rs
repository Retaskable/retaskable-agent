//! A temp checkout with a bare `origin`: `main` (c1), `production` at c1 and `staging` two commits ahead (c2, c3).
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use crate::config::{DeployConfig, DeployTarget};
use crate::promote::{DeployLog, Promoter};
use crate::test_support::write_executable;

/// 2023-11-14T22:13:20.000Z.
pub const NOW: u64 = 1_700_000_000_000;

pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

pub fn commit(dir: &Path, file: &str) -> String {
    std::fs::write(dir.join(file), file).unwrap();
    git(dir, &["add", file]);
    git(dir, &["commit", "-qm", file]);
    git(dir, &["rev-parse", "HEAD"])
}

pub struct Fx {
    pub root: tempfile::TempDir,
    pub origin: PathBuf,
    pub work: PathBuf,
    pub c1: String,
    pub c2: String,
    pub c3: String,
}

impl Fx {
    pub fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let (origin, work) = (root.path().join("origin.git"), root.path().join("work"));
        git(root.path(), &["init", "-q", "--bare", "-b", "main", "origin.git"]);
        // So origin's reflog says what was pushed, and from what.
        git(&origin, &["config", "core.logAllRefUpdates", "true"]);
        git(root.path(), &["clone", "-q", "origin.git", "work"]);
        git(&work, &["checkout", "-q", "-b", "main"]);
        let c1 = commit(&work, "a.txt");
        git(&work, &["push", "-q", "origin", "main"]);
        git(&work, &["push", "-q", "origin", "main:production"]);
        git(&work, &["checkout", "-q", "-b", "staging"]);
        let c2 = commit(&work, "b.txt");
        let c3 = commit(&work, "c.txt");
        git(&work, &["push", "-q", "origin", "staging"]);
        Self {
            root,
            origin,
            work,
            c1,
            c2,
            c3,
        }
    }

    pub fn dir(&self) -> PathBuf {
        self.root.path().join("project")
    }

    pub fn pending_file(&self) -> PathBuf {
        self.dir().join(crate::promote::log::FILE)
    }

    /// Where origin's `production` is now.
    pub fn production(&self) -> String {
        git(&self.origin, &["rev-parse", "production"])
    }

    /// Origin's `production` reflog, newest first.
    pub fn reflog(&self) -> Vec<String> {
        git(&self.origin, &["reflog", "show", "--format=%H", "production"])
            .lines()
            .map(String::from)
            .collect()
    }

    /// What the fake `gh` was asked, one call a line.
    pub fn gh_calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.root.path().join("gh.log"))
            .unwrap_or_default()
            .lines()
            // The fake is run once with no arguments when it is written.
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect()
    }

    /// A fake `gh` for `o/r`: this computer is signed in as `machine`, whose permissions are `(admin, push)` (or a
    /// failure when `None`), and each `(login, permission)` is what the collaborators call answers.
    pub fn gh(&self, machine: &str, perms: Option<(bool, bool)>, people: &[(&str, &str)]) -> PathBuf {
        let log = self.root.path().join("gh.log");
        let mut script = format!("#!/bin/sh\necho \"$*\" >> \"{}\"\ncase \"$*\" in\n", log.display());
        script += &format!("\"api user --jq .login\") echo '{machine}';;\n");
        match perms {
            Some((admin, push)) => {
                script +=
                    &format!("\"api repos/o/r --jq .permissions\") echo '{{\"admin\":{admin},\"push\":{push}}}';;\n")
            }
            None => script += "\"api repos/o/r --jq .permissions\") echo 'HTTP 403: nope' >&2; exit 1;;\n",
        }
        for (login, perm) in people {
            script +=
                &format!("\"api repos/o/r/collaborators/{login}/permission --jq .permission\") echo '{perm}';;\n");
        }
        script += "*) echo \"gh: unexpected $*\" >&2; exit 1;;\nesac\n";
        let path = self.root.path().join("gh");
        write_executable(&path, &script);
        path
    }

    /// The promote of `o/r` staging -> production, asking `gh`; a staging address when it has one.
    pub fn promoter(&self, gh: PathBuf, staging_url: Option<&str>) -> Promoter {
        let target = |branch: &str, url: Option<&str>| {
            Some(DeployTarget {
                branch: branch.into(),
                url: url.map(String::from),
                smoke: None,
                version_path: None,
                rollout: None,
            })
        };
        Promoter {
            repo: self.work.clone(),
            github_repo: "o/r".into(),
            deploy: DeployConfig {
                merges_to: None,
                staging: target("staging", staging_url),
                production: target("production", None),
            },
            gh,
            log: Arc::new(DeployLog::open(&self.pending_file())),
            now_ms: || NOW,
        }
    }
}

/// A fake staging site answering `GET /version.json` with `status` and `body`.
pub async fn staging(status: u16, body: &str) -> String {
    use axum::{Router, http::StatusCode, routing::get};
    let body = body.to_string();
    let app = Router::new().route(
        "/version.json",
        get(move || {
            let body = body.clone();
            async move { (StatusCode::from_u16(status).unwrap(), body) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}
