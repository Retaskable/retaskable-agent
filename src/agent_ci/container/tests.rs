use super::*;

#[test]
fn the_log_is_read_whole_up_to_its_last_eight_megabytes() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("a.log");
    std::fs::write(&p, "one\ntwo\n").unwrap();
    assert_eq!(read_log(&p), "one\ntwo\n");
    assert_eq!(read_log(&dir.path().join("none.log")), "");
    let big = "x".repeat(TAIL_READ_BYTES as usize + 10);
    std::fs::write(&p, big).unwrap();
    assert_eq!(read_log(&p).len() as u64, TAIL_READ_BYTES);
}

#[test]
fn a_log_starts_afresh_with_its_header() {
    let dir = tempfile::tempdir().unwrap();
    let a = start_log(dir.path(), "0123456789abcdef", "push", "header").unwrap();
    assert_eq!(a.file_name().unwrap(), "0123456789ab-push.log");
    std::fs::write(&a, "old run\n").unwrap();
    let b = start_log(dir.path(), "0123456789abcdef", "push", "header").unwrap();
    assert_eq!(std::fs::read_to_string(b).unwrap(), "header\n");
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
    use std::os::unix::fs::PermissionsExt;

    const TOKEN: &str = "ghp_faketokenvalue123";

    fn stub(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("docker");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn step<'a>(docker: &'a Path, log: &'a Path, token: Option<&'a str>, ceiling: Duration) -> Step<'a> {
        Step {
            docker: docker.as_os_str(),
            name: "agent-ci-x--abcdef01-push-check",
            args: vec!["run".into(), "--rm".into(), "-e".into(), "T".into()],
            token,
            log,
            ceiling,
        }
    }

    #[tokio::test]
    async fn only_the_token_asked_for_reaches_docker_and_the_output_is_logged() {
        let t = tempfile::tempdir().unwrap();
        let rec = t.path().join("rec");
        let docker = stub(
            t.path(),
            &format!(
                "echo \"$@\" > {0}.args; env > {0}.env; echo hi; echo err >&2; exit 3",
                rec.display()
            ),
        );
        // SAFETY: the test process sets these before spawning; the command clears them.
        unsafe { std::env::set_var("GH_TOKEN", "ghp_other") };
        // SAFETY: the test process sets these before spawning; the command clears them.
        unsafe { std::env::set_var("GITHUB_TOKEN", "ghp_other") };
        let log = t.path().join("a.log");
        std::fs::write(&log, "header\n").unwrap();
        let out = run(&step(&docker, &log, Some(TOKEN), CEILING)).await.unwrap();
        assert_eq!((out.exit_code, out.timed_out), (Some(3), false));
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(text.starts_with("header\n") && text.contains("hi\n") && text.contains("err\n"));
        let env = std::fs::read_to_string(t.path().join("rec.env")).unwrap();
        let args = std::fs::read_to_string(t.path().join("rec.args")).unwrap();
        assert!(env.contains(&format!("T={TOKEN}")) && !args.contains(TOKEN));
        assert!(!env.contains("GH_TOKEN") && !env.contains("GITHUB_TOKEN") && !env.contains("ghp_other"));
        // A step with no token has no `T` at all.
        run(&step(&docker, &log, None, CEILING)).await.unwrap();
        let env = std::fs::read_to_string(t.path().join("rec.env")).unwrap();
        assert!(!env.lines().any(|l| l.starts_with("T=")));
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
        let log = t.path().join("a.log");
        let out = run(&step(&docker, &log, None, Duration::from_millis(300)))
            .await
            .unwrap();
        assert!(out.timed_out);
        assert_eq!(out.exit_code, None);
        assert_eq!(
            std::fs::read_to_string(rec).unwrap().trim(),
            "rm -f agent-ci-x--abcdef01-push-check"
        );
    }

    #[tokio::test]
    async fn leftovers_with_the_prefix_are_removed_and_none_is_quiet() {
        let t = tempfile::tempdir().unwrap();
        let rec = t.path().join("calls");
        let none = t.path().join("none");
        let docker = stub(
            t.path(),
            &format!(
                "echo \"$@\" >> {}; if [ \"$1\" = ps ]; then [ -f {} ] || printf 'c1\\nc2\\n'; fi",
                rec.display(),
                none.display()
            ),
        );
        remove_leftovers(docker.as_os_str(), "agent-ci-box-o-p--").await;
        let calls = std::fs::read_to_string(&rec).unwrap();
        assert_eq!(
            calls.lines().collect::<Vec<_>>(),
            ["ps -aq --filter name=^agent-ci-box-o-p--", "rm -f c1 c2"]
        );
        std::fs::write(&none, "").unwrap();
        std::fs::remove_file(&rec).unwrap();
        remove_leftovers(docker.as_os_str(), "agent-ci-box-o-p--").await;
        assert_eq!(std::fs::read_to_string(&rec).unwrap().lines().count(), 1);
    }
}
