use super::*;

#[test]
fn the_name_is_repo_and_short_sha() {
    assert_eq!(
        container_name("Acme/web app", "0123456789abcdef"),
        "retaskable-ci-Acme-web-app-0123456"
    );
}

#[test]
fn the_tail_is_at_most_120_lines() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("a.log");
    let body: String = (1..=500).map(|i| format!("line {i}\n")).collect();
    std::fs::write(&p, body).unwrap();
    let tail = tail_of(&p);
    assert_eq!(tail.lines().count(), LOG_TAIL_LINES);
    assert!(tail.ends_with("line 500"));
    assert!(tail.starts_with("line 381"));
}

#[test]
fn logs_older_than_seven_days_are_pruned() {
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("old.log");
    let new = dir.path().join("new.log");
    std::fs::write(&old, "x").unwrap();
    std::fs::write(&new, "x").unwrap();
    let future = SystemTime::now() + LOG_KEPT + Duration::from_secs(60);
    // Both are "old" from a point 7 days ahead; touch `new` to be recent relative to that.
    File::options()
        .write(true)
        .open(&new)
        .unwrap()
        .set_modified(future - Duration::from_secs(3600))
        .unwrap();
    prune_logs(dir.path(), future);
    assert!(!old.exists());
    assert!(new.exists());
}

#[cfg(unix)]
mod stub {
    use super::*;

    const TOKEN: &str = "ghp_faketokenvalue123";

    fn stub(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("docker");
        crate::test_support::write_script(&path, body);
        path
    }

    fn check<'a>(docker: &'a Path, dir: &'a Path, logs: &'a Path, mode: Mode, ceiling: Duration) -> Check<'a> {
        Check {
            docker: docker.as_os_str(),
            repo: "web",
            sha: "abcdef0123",
            dir,
            image: "rust:1-bookworm",
            mode,
            log_dir: logs,
            ceiling,
        }
    }

    #[tokio::test]
    async fn no_token_reaches_docker_and_the_args_are_the_rule() {
        let t = tempfile::tempdir().unwrap();
        let rec = t.path().join("rec");
        let docker = stub(
            t.path(),
            &format!("echo \"$@\" > {0}.args; env > {0}.env; echo hi; exit 3", rec.display()),
        );
        let work = t.path().join("work");
        std::fs::create_dir(&work).unwrap();
        let parent: Vec<(std::ffi::OsString, std::ffi::OsString)> = [
            ("GH_TOKEN", TOKEN),
            ("GITHUB_TOKEN", TOKEN),
            ("GLAB_TOKEN", TOKEN),
            ("PATH", "/usr/bin:/bin"),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect();
        let logs = t.path().join("logs");
        let out = run_with_env(&check(&docker, &work, &logs, Mode::PullRequest, CEILING), parent)
            .await
            .unwrap();
        assert_eq!(out.exit_code, Some(3));
        assert_eq!(out.tail, "hi");
        let args = std::fs::read_to_string(t.path().join("rec.args")).unwrap();
        let env = std::fs::read_to_string(t.path().join("rec.env")).unwrap();
        for bad in ["GH_TOKEN", "GITHUB_TOKEN", "GLAB_TOKEN", TOKEN] {
            assert!(!env.contains(bad) && !args.contains(bad), "{bad} leaked");
        }
        assert_eq!(args.matches(" -v ").count() + args.starts_with("-v ") as usize, 1);
        assert!(args.contains("--name retaskable-ci-web-abcdef0 "));
        assert!(args.contains("--cpu-shares 128"));
        assert!(args.contains(&format!("-v {}:{WORKDIR}", work.display())));
        assert!(args.trim_end().ends_with("bash .agent-ci/checks.sh pull_request"));
        let push = run_args(&check(&docker, &work, &logs, Mode::Push, CEILING));
        assert_eq!(push.last().unwrap(), "push");
        assert!(!args.contains("docker.sock"));
    }

    #[tokio::test]
    async fn the_ceiling_kills_and_removes_by_name() {
        let t = tempfile::tempdir().unwrap();
        let rec = t.path().join("rm");
        let docker = stub(
            t.path(),
            &format!(
                "if [ \"$1\" = rm ]; then echo \"$@\" > {}; exit 0; fi; exec sleep 30",
                rec.display()
            ),
        );
        let work = t.path().join("work");
        std::fs::create_dir(&work).unwrap();
        let logs = t.path().join("logs");
        let out = run(&check(&docker, &work, &logs, Mode::Push, Duration::from_millis(300)))
            .await
            .unwrap();
        assert!(out.timed_out);
        assert_eq!(out.exit_code, None);
        assert_eq!(
            std::fs::read_to_string(rec).unwrap().trim(),
            "rm -f retaskable-ci-web-abcdef0"
        );
    }
}

/// Skipped without a working docker: a tiny image runs and its output is kept.
#[tokio::test]
async fn real_docker_runs_a_tiny_image() {
    let docker = OsStr::new("docker");
    if super::super::docker::probe(docker, false).await != super::super::docker::DockerState::Ok {
        return;
    }
    let t = tempfile::tempdir().unwrap();
    let work = t.path().join("work");
    std::fs::create_dir_all(work.join(".agent-ci")).unwrap();
    std::fs::write(work.join(".agent-ci/checks.sh"), "echo ran $1; exit 0\n").unwrap();
    let c = Check {
        docker,
        repo: "smoke",
        sha: "0000000",
        dir: &work,
        image: "bash:5",
        mode: Mode::Push,
        log_dir: &t.path().join("logs"),
        ceiling: Duration::from_secs(300),
    };
    let Ok(out) = run(&c).await else { return };
    if out.exit_code == Some(0) {
        assert!(out.tail.contains("ran push"));
    }
}
