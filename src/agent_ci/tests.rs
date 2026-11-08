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
}
