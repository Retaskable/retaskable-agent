use super::*;
use crate::lane_pool::{Holder, LanePool, PoolRules};
use crate::orchestrator::live::{HeldLane, LiveHandle};

#[test]
fn ready_with_ci_when_ready_takes_only_ci_ready_pull_requests_and_off_takes_non_drafts() {
    let json = r#"[
      {"number":1,"isDraft":false,"labels":[{"name":"ci-ready"}],"headRefOid":"aaa"},
      {"number":2,"isDraft":false,"labels":[],"headRefOid":"bbb"},
      {"number":3,"isDraft":true,"labels":[{"name":"ci-ready"}],"headRefOid":"ccc"},
      {"number":4,"isDraft":true,"labels":[],"headRefOid":"ddd"}]"#;
    let nums = |on| {
        prs::ready(json, "m", on)
            .unwrap()
            .iter()
            .map(|k| k.pr)
            .collect::<Vec<_>>()
    };
    assert_eq!(nums(true), [1, 3]);
    assert_eq!(nums(false), [1, 2]);
    assert!(prs::ready("nope", "m", false).is_err());
}

#[test]
fn a_new_main_sha_is_a_new_key() {
    let json = r#"[{"number":1,"isDraft":false,"labels":[],"headRefOid":"aaa"}]"#;
    let a = prs::ready(json, "m1", false).unwrap();
    let b = prs::ready(json, "m2", false).unwrap();
    assert_ne!(a, b);
    assert_eq!(a, prs::ready(json, "m1", false).unwrap());
    assert_eq!((a[0].head.as_str(), a[0].main.as_str()), ("aaa", "m1"));
}

#[test]
fn a_reason_is_said_once_across_polls() {
    let mut ci = CiLoop::new(Programs::default(), LiveHandle::default(), false);
    assert!(ci.say_once("p", "No image"));
    assert!(!ci.say_once("p", "No image"));
    assert!(ci.say_once("p", "No checks.sh"));
}

#[test]
fn lanes_all_busy_does_not_block_a_permit() {
    let mut pool = LanePool::new(PoolRules {
        lanes: 2,
        ..Default::default()
    });
    for k in 1..=2 {
        assert!(pool.take(
            k,
            Holder::Item {
                project: "p".into(),
                item: format!("i{k}")
            }
        ));
    }
    let mut ci = CiLoop::new(Programs::default(), LiveHandle::default(), false);
    ci.resize(1);
    let permit = ci.slot().try_acquire_owned().expect("a permit with every lane busy");
    assert_eq!(ci.slot().available_permits(), 0);
    drop(permit);
    ci.resize(0);
    assert!(ci.slot().try_acquire_owned().is_err());
}

#[tokio::test]
async fn no_slots_or_no_docker_means_zero_permits_and_a_reason() {
    let env = Env::from_map(Default::default());
    let mut cfg = ConfigFile::default();
    let live = LiveHandle::default();
    let mut ci = CiLoop::new(
        Programs {
            docker: "retaskable-no-such-docker".into(),
            ..Programs::default()
        },
        live.clone(),
        false,
    );
    cfg.machine.ci_slots = 2;
    ci.poll(&cfg, &env).await;
    let c = live.snapshot().ci.unwrap();
    assert_eq!((c.slots, c.running, c.docker), (2, 0, "missing"));
    assert!(c.docker_reason.unwrap().contains("isn't installed"));
    assert_eq!(ci.slot().available_permits(), 0);

    cfg.machine.ci_slots = 0;
    ci.poll(&cfg, &env).await;
    let c = live.snapshot().ci.unwrap();
    assert_eq!((c.slots, c.docker), (0, "off"));
    assert!(c.docker_reason.unwrap().contains("CI slots is 0"));
    assert_eq!(ci.slot().available_permits(), 0);
}

#[test]
fn the_ticks_ci_object_carries_slots_running_and_docker() {
    let live = LiveHandle::default();
    live.update_ci(|c| {
        c.slots = 1;
        c.docker = "ok";
    });
    assert_eq!(
        live.snapshot().ci.unwrap().tick_json(),
        json!({ "slots": 1, "running": 0, "docker": "ok" })
    );
}

#[cfg(unix)]
mod stubbed {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    fn run(dir: &Path, args: &[&str]) {
        let out = Command::new("git").args(args).current_dir(dir).output().unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn script(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// A clone whose `origin/main` holds `files` (and nothing else), plus a branch `pr` that adds `branch_files`.
    fn checkout(root: &Path, files: &[&str], branch_files: &[&str]) -> std::path::PathBuf {
        let origin = root.join("origin.git");
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        run(root, &["init", "-q", "--bare", "-b", "main", origin.to_str().unwrap()]);
        run(&work, &["init", "-q", "-b", "main"]);
        run(&work, &["config", "user.email", "t@t"]);
        run(&work, &["config", "user.name", "t"]);
        let put = |names: &[&str]| {
            for n in names {
                std::fs::create_dir_all(work.join(".agent-ci")).unwrap();
                std::fs::write(work.join(".agent-ci").join(n), "rust:1\n").unwrap();
            }
        };
        std::fs::write(work.join("a"), "a").unwrap();
        put(files);
        run(&work, &["add", "."]);
        run(&work, &["commit", "-qm", "main"]);
        run(&work, &["remote", "add", "origin", origin.to_str().unwrap()]);
        run(&work, &["push", "-q", "origin", "main"]);
        run(&work, &["checkout", "-qb", "pr"]);
        put(branch_files);
        run(&work, &["add", "."]);
        run(&work, &["commit", "-qm", "pr", "--allow-empty"]);
        run(&work, &["push", "-q", "origin", "pr"]);
        work
    }

    struct Rig {
        _dir: tempfile::TempDir,
        ci: CiLoop,
        live: LiveHandle,
        cfg: ConfigFile,
    }

    fn rig(files: &[&str], branch_files: &[&str], ci_when_ready: bool, gh_json: &str) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let work = checkout(dir.path(), files, branch_files);
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let docker = script(&bin, "docker", "exit 0");
        let gh = script(&bin, "gh", &format!("cat <<'EOF'\n{gh_json}\nEOF"));
        let live = LiveHandle::default();
        live.ticked("p", false, "t".into(), Vec::<HeldLane>::new(), "d".into());
        let mut cfg = ConfigFile::default();
        cfg.projects.push(ProjectConfig {
            id: Some("p".into()),
            repo_path: work.to_str().unwrap().into(),
            github_repo: "o/p".into(),
            base_branch: "main".into(),
            require_ci: ci_when_ready,
            ci_when_ready,
            ..Default::default()
        });
        let programs = Programs {
            docker: docker.into(),
            gh: gh.into(),
            ..Programs::default()
        };
        Rig {
            ci: CiLoop::new(programs, live.clone(), false),
            live,
            cfg,
            _dir: dir,
        }
    }

    const ONE_PR: &str = r#"[{"number":7,"isDraft":false,"labels":[],"headRefOid":"h1"}]"#;

    #[tokio::test]
    async fn both_files_on_main_give_the_project_agent_ci_and_its_keys() {
        let mut r = rig(&["image", "checks.sh"], &[], false, ONE_PR);
        let keys = r.ci.poll(&r.cfg, &Env::from_map(Default::default())).await;
        assert_eq!(keys["p"].len(), 1);
        assert_eq!(keys["p"][0].pr, 7);
        assert_eq!(keys["p"][0].main.len(), 40);
        let c = r.live.snapshot().ci.unwrap();
        assert_eq!((c.docker, c.running, c.slots), ("ok", 0, 1));
        assert!(c.reasons.is_empty());
        assert_eq!(r.ci.slot().available_permits(), 1);
    }

    #[tokio::test]
    async fn one_file_missing_says_why_once_and_keeps_the_reason() {
        let mut r = rig(&["image"], &[], false, ONE_PR);
        for _ in 0..3 {
            let keys = r.ci.poll(&r.cfg, &Env::from_map(Default::default())).await;
            assert!(!keys.contains_key("p"));
        }
        let reasons = r.live.snapshot().ci.unwrap().reasons;
        assert!(reasons["p"].contains(".agent-ci/checks.sh"), "{reasons:?}");
        assert_eq!(r.ci.logged["p"], reasons["p"]);
    }

    #[tokio::test]
    async fn files_only_on_a_pull_request_branch_are_ignored() {
        let mut r = rig(&[], &["image", "checks.sh"], false, ONE_PR);
        let keys = r.ci.poll(&r.cfg, &Env::from_map(Default::default())).await;
        assert!(keys.is_empty());
        assert!(r.live.snapshot().ci.unwrap().reasons["p"].contains("no agent CI"));
    }

    #[tokio::test]
    async fn a_project_whose_backlog_cant_be_reached_runs_no_ci() {
        let mut r = rig(&["image", "checks.sh"], &[], false, ONE_PR);
        r.live.failed("p", "t2".into(), "down".into());
        let keys = r.ci.poll(&r.cfg, &Env::from_map(Default::default())).await;
        assert!(keys.is_empty());
        assert!(r.live.snapshot().ci.unwrap().reasons["p"].contains("backlog can't be reached"));
    }

    #[tokio::test]
    async fn listing_follows_ci_when_ready() {
        let json = r#"[{"number":1,"isDraft":false,"labels":[{"name":"ci-ready"}],"headRefOid":"a"},
          {"number":2,"isDraft":false,"labels":[],"headRefOid":"b"}]"#;
        let mut r = rig(&["image", "checks.sh"], &[], true, json);
        let keys = r.ci.poll(&r.cfg, &Env::from_map(Default::default())).await;
        assert_eq!(keys["p"].iter().map(|k| k.pr).collect::<Vec<_>>(), [1]);
    }

    /// A rig whose origin serves pull requests 7 and 8 (a clean change) and 9 (conflicting with main), with a `gh`
    /// that logs its `api` calls and a `docker` that logs its calls and exits with `docker_body`'s status.
    fn pr_rig(docker_body: &str) -> Rig {
        let mut r = rig(
            &["image", "checks.sh"],
            &[],
            false,
            r#"[{"number":7,"isDraft":false,"labels":[],"headRefOid":"h7"},
                {"number":8,"isDraft":false,"labels":[],"headRefOid":"h8"}]"#,
        );
        let root = r._dir.path().to_path_buf();
        let (work, origin) = (root.join("work"), root.join("origin.git"));
        std::fs::write(work.join("a"), "pr").unwrap();
        run(&work, &["commit", "-qam", "pr change"]);
        run(&work, &["push", "-q", "origin", "pr"]);
        run(&work, &["checkout", "-q", "main"]);
        std::fs::write(work.join("a"), "main").unwrap();
        run(&work, &["commit", "-qam", "main change"]);
        run(&work, &["push", "-q", "origin", "main"]);
        // 7 and 8 are clean (a branch off the old main touching another file), 9 conflicts.
        run(&work, &["checkout", "-qb", "clean", "HEAD~1"]);
        std::fs::write(work.join("b"), "b").unwrap();
        run(&work, &["add", "b"]);
        run(&work, &["commit", "-qm", "clean"]);
        run(&work, &["push", "-q", "origin", "clean"]);
        run(&origin, &["update-ref", "refs/pull/7/head", "refs/heads/clean"]);
        run(&origin, &["update-ref", "refs/pull/8/head", "refs/heads/clean"]);
        run(&origin, &["update-ref", "refs/pull/9/head", "refs/heads/pr"]);
        let bin = root.join("bin");
        let log = root.join("calls.log");
        script(
            &bin,
            "docker",
            &format!(
                "echo \"docker $*\" >> {l}\n[ \"$1\" = run ] || exit 0\n{docker_body}",
                l = log.display()
            ),
        );
        let json = r#"[{"number":7,"isDraft":false,"labels":[],"headRefOid":"h7"}]"#;
        script(
            &bin,
            "gh",
            &format!(
                "if [ \"$1\" = api ]; then echo \"gh $*\" >> {l}; [ \"$3\" = GET ] && echo '[]'; exit 0; fi\ncat <<'EOF'\n{json}\nEOF",
                l = log.display()
            ),
        );
        r.ci.programs.docker = bin.join("docker").into();
        r.ci.programs.gh = bin.join("gh").into();
        r
    }

    fn key(pr: u64, head: &str) -> CheckKey {
        CheckKey {
            pr,
            head: head.into(),
            main: "m".repeat(40),
        }
    }

    async fn go(r: &mut Rig, keys: Vec<CheckKey>) -> Vec<tokio::task::JoinHandle<()>> {
        let env = Env::from_map(Default::default());
        r.ci.poll(&r.cfg, &env).await;
        let data = r._dir.path().join("data");
        r.ci.start(&r.cfg, &env, &data, [("p".to_string(), keys)].into()).await
    }

    fn calls(r: &Rig) -> String {
        std::fs::read_to_string(r._dir.path().join("calls.log")).unwrap_or_default()
    }

    fn events(r: &Rig) -> String {
        let dir = r._dir.path().join("data/projects/p/events");
        std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| std::fs::read_to_string(e.path()).unwrap())
            .collect()
    }

    #[tokio::test]
    async fn a_ready_pull_request_gets_pending_then_success_and_the_events() {
        let mut r = pr_rig("exit 0");
        for t in go(&mut r, vec![key(7, "h7")]).await {
            t.await.unwrap();
        }
        let c = calls(&r);
        let (pending, success) = (c.find("state=pending").unwrap(), c.find("state=success").unwrap());
        assert!(
            pending < success && !c.contains("state=failure") && !c.contains("/comments"),
            "{c}"
        );
        assert!(
            c.contains("docker run") && c.contains("--name retaskable-ci-o-p-h7"),
            "{c}"
        );
        let ev = events(&r);
        assert!(
            ev.contains(r#""state":"started""#) && ev.contains(r#""state":"finished""#),
            "{ev}"
        );
        assert!(ev.contains("PR #7") && ev.contains(r#""key":"h7-mmmmmmm""#), "{ev}");
        let ci = r.live.snapshot().ci.unwrap();
        assert!(ci.checks.is_empty() && ci.last_result.unwrap()["ok"] == true);
        assert_eq!(
            std::fs::read_dir(r._dir.path().join("data/projects/p/ci"))
                .unwrap()
                .count(),
            0
        );
        // A key no longer listed (this one isn't) is forgotten at the next poll.
        let env = Env::from_map(Default::default());
        r.ci.poll(&r.cfg, &env).await;
        assert!(r.ci.seen.is_empty());
    }

    #[tokio::test]
    async fn a_failing_check_posts_failure_and_one_comment() {
        let mut r = pr_rig("echo boom; exit 1");
        for t in go(&mut r, vec![key(7, "h7")]).await {
            t.await.unwrap();
        }
        let c = calls(&r);
        assert!(c.contains("state=failure") && !c.contains("state=success"), "{c}");
        assert_eq!(c.matches("issues/7/comments").count(), 2, "{c}"); // the lookup and the one post
        assert!(c.contains("body=agent CI failed on h7."), "{c}");
        assert!(c.contains("boom"), "{c}");
    }

    #[tokio::test]
    async fn a_conflicting_pull_request_fails_without_a_container() {
        let mut r = pr_rig("exit 0");
        for t in go(&mut r, vec![key(9, "h9")]).await {
            t.await.unwrap();
        }
        let c = calls(&r);
        assert!(c.contains("state=failure") && c.contains("conflicts with main"), "{c}");
        assert!(!c.contains("docker run"), "{c}");
        assert!(c.contains("issues/9/comments"), "{c}");
    }

    #[tokio::test]
    async fn one_slot_runs_two_pull_requests_one_after_the_other() {
        let mut r = pr_rig("exit 0");
        let log = r._dir.path().join("calls.log");
        let body = format!("echo begin >> {l}; sleep 0.4; echo end >> {l}", l = log.display());
        script(
            &r._dir.path().join("bin"),
            "docker",
            &format!(
                "echo \"docker $*\" >> {l}\n[ \"$1\" = run ] || exit 0\n{body}",
                l = log.display()
            ),
        );
        let keys = vec![key(7, "h7"), key(8, "h8")];
        let first = go(&mut r, keys.clone()).await;
        assert_eq!(first.len(), 1, "only one permit");
        let c = r.live.snapshot().ci.unwrap();
        assert_eq!(c.slots, 1);
        // Still running: the Now tab lists it and the second key finds no permit.
        tokio::time::sleep(Duration::from_millis(150)).await;
        let live = r.live.snapshot().ci.unwrap();
        assert_eq!((live.running, live.checks.len(), live.checks[0].pr), (1, 1, 7));
        assert!(go(&mut r, keys.clone()).await.is_empty());
        for t in first {
            t.await.unwrap();
        }
        let second = go(&mut r, keys).await;
        assert_eq!(second.len(), 1);
        for t in second {
            t.await.unwrap();
        }
        let lines: Vec<_> = calls(&r)
            .lines()
            .filter(|l| *l == "begin" || *l == "end")
            .map(String::from)
            .collect();
        assert_eq!(lines, ["begin", "end", "begin", "end"]);
    }

    #[tokio::test]
    async fn recovery_removes_a_leftover_directory_and_its_container() {
        let r = pr_rig("exit 0");
        let root = r._dir.path().join("data/projects/p/ci");
        std::fs::create_dir_all(root.join("check-1")).unwrap();
        std::fs::write(root.join("check-1/f"), "x").unwrap();
        std::fs::write(root.join("check-1.name"), "retaskable-ci-o-p-abc1234").unwrap();
        r.ci.recover(&r.cfg, &r._dir.path().join("data")).await;
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        assert!(
            calls(&r).contains("docker rm -f retaskable-ci-o-p-abc1234"),
            "{}",
            calls(&r)
        );
    }

    #[tokio::test]
    async fn a_shrink_while_a_check_runs_does_not_leave_extra_permits() {
        let mut r = pr_rig("sleep 0.4");
        let env = Env::from_map(Default::default());
        let tasks = go(&mut r, vec![key(7, "h7")]).await;
        // Docker "down" for one poll while the check holds the only permit.
        r.cfg.machine.ci_slots = 0;
        r.ci.poll(&r.cfg, &env).await;
        for t in tasks {
            t.await.unwrap();
        }
        assert_eq!(r.ci.slot().available_permits(), 0);
        r.cfg.machine.ci_slots = 1;
        r.ci.poll(&r.cfg, &env).await;
        assert_eq!(r.ci.slot().available_permits(), 1);
        assert_eq!(r.live.snapshot().ci.unwrap().running, 0);
    }
}
