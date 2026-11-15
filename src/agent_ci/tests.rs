use super::*;
#[cfg(unix)]
use serde_json::json;
#[cfg(unix)]
use store::tests::FakeStore;

#[test]
fn metas_carry_what_a_claim_tells_the_store_and_the_automerge_label() {
    let json = r#"[{"number":4,"labels":[{"name":"automerge"}],"headRefOid":"a","headRefName":"fix","title":"T","author":{"login":"me"}},
      {"number":5,"labels":[],"headRefOid":"b"}]"#;
    let m = prs::metas(json).unwrap();
    assert_eq!(
        m[&4],
        prs::PrMeta {
            branch: "fix".into(),
            title: "T".into(),
            author: "me".into(),
            automerge: true
        }
    );
    assert_eq!(m[&5], prs::PrMeta::default());
    assert!(prs::metas("nope").is_err());
}

#[test]
fn ready_skips_drafts_and_forks_and_holds_an_agent_branch_until_ci_ready() {
    let json = r#"[
      {"number":1,"isDraft":false,"labels":[],"headRefOid":"aaa","headRefName":"fix-it"},
      {"number":2,"isDraft":true,"labels":[],"headRefOid":"bbb","headRefName":"wip"},
      {"number":3,"isDraft":false,"labels":[],"headRefOid":"ccc","headRefName":"patch","isCrossRepository":true},
      {"number":4,"isDraft":false,"labels":[],"headRefOid":"ddd","headRefName":"retaskable/item-4"},
      {"number":5,"isDraft":false,"labels":[{"name":"ci-ready"}],"headRefOid":"eee","headRefName":"retaskable/item-5"},
      {"number":6,"isDraft":true,"labels":[{"name":"ci-ready"}],"headRefOid":"fff","headRefName":"retaskable/item-6"}]"#;
    let nums: Vec<u64> = prs::ready(json, "m").unwrap().iter().map(|k| k.pr).collect();
    assert_eq!(nums, [1, 5]);
    assert!(prs::ready("nope", "m").is_err());
}

#[test]
fn a_new_main_sha_is_a_new_key() {
    let json = r#"[{"number":1,"isDraft":false,"labels":[],"headRefOid":"aaa"}]"#;
    let a = prs::ready(json, "m1").unwrap();
    let b = prs::ready(json, "m2").unwrap();
    assert_ne!(a, b);
    assert_eq!(a, prs::ready(json, "m1").unwrap());
    assert_eq!((a[0].head.as_str(), a[0].main.as_str()), ("aaa", "m1"));
}

#[test]
fn a_reason_is_said_once_across_polls() {
    let mut ci = CiLoop::new(Programs::default());
    assert!(ci.say_once("p", "No image"));
    assert!(!ci.say_once("p", "No image"));
    assert!(ci.say_once("p", "No checks.sh"));
}

#[test]
fn resize_gives_and_takes_back_permits() {
    let mut ci = CiLoop::new(Programs::default());
    ci.resize(1);
    let permit = ci.slot().try_acquire_owned().expect("a permit");
    assert_eq!(ci.slot().available_permits(), 0);
    drop(permit);
    ci.resize(0);
    assert!(ci.slot().try_acquire_owned().is_err());
}

fn settings(slots: u32) -> Settings {
    Settings {
        repos: vec![Repo::parse("o/p").unwrap()],
        slots,
        cpus: 2,
        computer: "t".into(),
        data_dir: std::env::temp_dir().join("retaskable-ci-test-never-written"),
    }
}

#[tokio::test]
async fn no_slots_or_no_docker_means_zero_permits_and_a_reason() {
    let mut ci = CiLoop::new(Programs {
        docker: "retaskable-no-such-docker".into(),
        ..Programs::default()
    });
    ci.poll(&settings(2)).await;
    assert!(ci.logged["docker"].contains("isn't installed"));
    assert_eq!(ci.slot().available_permits(), 0);

    ci.poll(&settings(0)).await;
    assert!(ci.logged["docker"].contains("CI slots is 0"));
    assert_eq!(ci.slot().available_permits(), 0);
}

#[test]
fn a_checks_cpus_are_capped_at_what_docker_has() {
    let mut ci = CiLoop::new(Programs::default());
    assert_eq!(ci.cpus(&settings(1)), 2);
    ci.ncpu = Some(1);
    assert_eq!(ci.cpus(&settings(1)), 1);
    ci.ncpu = Some(16);
    assert_eq!(ci.cpus(&settings(1)), 2);
}

#[cfg(unix)]
mod stubbed {
    use super::*;
    use std::path::Path;
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
        crate::test_support::write_executable(&path, &format!("#!/bin/sh\n{body}\n"));
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
        cfg: Settings,
        store: Arc<FakeStore>,
    }

    fn rig(files: &[&str], branch_files: &[&str], gh_json: &str) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        checkout(dir.path(), files, branch_files);
        let origin = dir.path().join("origin.git");
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let docker = script(&bin, "docker", "exit 0");
        let gh = script(&bin, "gh", &format!("cat <<'EOF'\n{gh_json}\nEOF"));
        let cfg = Settings {
            repos: vec![Repo::parse("o/p").unwrap()],
            slots: 1,
            cpus: 2,
            computer: "t".into(),
            data_dir: dir.path().join("data"),
        };
        let programs = Programs {
            docker: docker.into(),
            gh: gh.into(),
            ..Programs::default()
        };
        let store = FakeStore::winning();
        let mut ci = CiLoop::new(programs).with_store(store.clone());
        ci.finish_pause = Duration::from_millis(1);
        let url = origin.to_str().unwrap().to_string();
        ci.remote = Arc::new(move |_| url.clone());
        Rig {
            ci,
            cfg,
            store,
            _dir: dir,
        }
    }

    const ONE_PR: &str = r#"[{"number":7,"isDraft":false,"labels":[],"headRefOid":"h1"}]"#;

    #[tokio::test]
    async fn both_files_on_main_give_the_repo_agent_ci_and_its_keys() {
        let mut r = rig(&["image", "checks.sh"], &[], ONE_PR);
        let keys = r.ci.poll(&r.cfg).await;
        assert_eq!(keys["o-p"].len(), 2);
        assert_eq!(keys["o-p"][0].pr, 7);
        assert_eq!(keys["o-p"][0].main.len(), 40);
        assert!(keys["o-p"][1].is_push() && keys["o-p"][1].head == keys["o-p"][0].main);
        assert!(r.ci.logged.is_empty());
        assert_eq!(r.ci.slot().available_permits(), 1);
        assert!(r.cfg.mirror(&r.cfg.repos[0]).join("HEAD").exists());
    }

    #[tokio::test]
    async fn one_file_missing_says_why_once_and_keeps_the_reason() {
        let mut r = rig(&["image"], &[], ONE_PR);
        for _ in 0..3 {
            let keys = r.ci.poll(&r.cfg).await;
            assert!(!keys.contains_key("o-p"));
        }
        assert!(r.ci.logged["o-p"].contains(".agent-ci/checks.sh"), "{:?}", r.ci.logged);
    }

    #[tokio::test]
    async fn files_only_on_a_pull_request_branch_are_ignored() {
        let mut r = rig(&[], &["image", "checks.sh"], ONE_PR);
        let keys = r.ci.poll(&r.cfg).await;
        assert!(keys.is_empty());
        assert!(r.ci.logged["o-p"].contains("no agent CI"), "{:?}", r.ci.logged);
    }

    /// A rig whose origin serves pull requests 7 and 8 (a clean change) and 9 (conflicting with main), with a `gh`
    /// that logs its `api` calls and a `docker` that logs its calls and exits with `docker_body`'s status.
    fn pr_rig(docker_body: &str) -> Rig {
        let mut r = rig(
            &["image", "checks.sh"],
            &[],
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
        script(&bin, "docker", &docker_stub(&log, docker_body, ""));
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

    /// A `docker` that logs each call and runs `body` for the check container only (fetch and warm succeed); a
    /// `ps` answers `ps_ids`. `body` may call `wait_gate`, which holds the check until the test calls `open_gate`.
    fn docker_stub(log: &Path, body: &str, ps_ids: &str) -> String {
        format!(
            "echo \"docker $*\" >> {l}\n[ \"$1\" = ps ] && echo '{ps_ids}'\n[ \"$1\" = run ] || exit 0\n\
             n=; p=; for a in \"$@\"; do [ \"$p\" = --name ] && n=$a; p=$a; done\n\
             case \"$n\" in *-check) ;; *) exit 0;; esac\n\
             wait_gate() {{ while [ ! -f {g} ]; do sleep 0.01; done; }}\n{body}",
            l = log.display(),
            g = log.with_file_name("gate").display()
        )
    }

    /// Lets every check held at `wait_gate` finish (and any later one run straight through).
    fn open_gate(r: &Rig) {
        std::fs::write(r._dir.path().join("gate"), "").unwrap();
    }

    /// Waits until `cond` holds, failing after ten seconds rather than hanging.
    async fn until(what: &str, mut cond: impl FnMut() -> bool) {
        let started = std::time::Instant::now();
        while !cond() {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "timed out waiting for {what}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn key(pr: u64, head: &str) -> CheckKey {
        CheckKey {
            pr,
            head: head.into(),
            main: "m".repeat(40),
        }
    }

    async fn go(r: &mut Rig, keys: Vec<CheckKey>) -> Vec<tokio::task::JoinHandle<()>> {
        r.ci.poll(&r.cfg).await;
        r.ci.start(&r.cfg, [("o-p".to_string(), keys)].into()).await
    }

    fn calls(r: &Rig) -> String {
        std::fs::read_to_string(r._dir.path().join("calls.log")).unwrap_or_default()
    }

    fn events(r: &Rig) -> String {
        let dir = r._dir.path().join("data/projects/o-p/events");
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
            c.contains("--name agent-ci-t-o-p--h7-pull_request-fetch")
                && c.contains("--name agent-ci-t-o-p--h7-pull_request-check")
                && !c.contains("-warm")
                && c.contains("com.docker.compose.project=retaskable-ci"),
            "{c}"
        );
        assert_eq!(check_runs(&r), 1);
        let log = std::fs::read_dir(r._dir.path().join("data/projects/o-p/ci-logs"))
            .unwrap()
            .flatten()
            .next()
            .expect("a log");
        assert!(log.file_name().to_string_lossy().ends_with("-pull_request.log"));
        let ev = events(&r);
        assert!(
            ev.contains(r#""state":"started""#) && ev.contains(r#""state":"finished""#),
            "{ev}"
        );
        assert!(ev.contains("PR #7") && ev.contains(r#""key":"h7-mmmmmmm""#), "{ev}");
        let claim = &r.store.args("ci_claim")[0];
        assert_eq!(claim["sha"], "h7");
        assert_eq!(claim["event"], "pull_request");
        assert_eq!((claim["pr"].as_u64(), claim["computer"].as_str()), (Some(7), Some("t")));
        assert_eq!(
            claim["slots"].as_u64(),
            Some(1),
            "the runner's slots, for the Agents tab"
        );
        let fin = &r.store.args("ci_finish")[0];
        assert_eq!(
            (fin["result"].as_str(), fin["context"].as_str()),
            (Some("passed"), Some("ci/agent/pr/ci"))
        );
        // A key no longer listed (this one isn't) is forgotten at the next poll.
        r.ci.poll(&r.cfg).await;
        assert!(r.ci.seen.lock().unwrap().is_empty());
    }

    /// `pr_rig` with a `gh` that logs every call and answers the pull request's own endpoint with `view`.
    fn automerge_rig(docker_body: &str, view: &str) -> Rig {
        let r = pr_rig(docker_body);
        let bin = r._dir.path().join("bin");
        let log = r._dir.path().join("calls.log");
        script(
            &bin,
            "gh",
            &format!(
                "echo \"gh $*\" >> {l}\nif [ \"$1\" = api ]; then\n case \"$4\" in\n repos/o/p/pulls/7) echo '{view}';;\n \
                 *) [ \"$3\" = GET ] && echo '[]';;\n esac\n exit 0\nfi\n\
                 echo '[{{\"number\":7,\"isDraft\":false,\"labels\":[],\"headRefOid\":\"h7\"}}]'",
                l = log.display()
            ),
        );
        r
    }

    const LABELLED: &str = r#"{"state":"open","head":{"sha":"h7","ref":"fix"},"labels":[{"name":"automerge"}]}"#;

    #[tokio::test]
    async fn a_green_automerge_check_merges_and_a_red_one_does_not() {
        for (docker, merged) in [("exit 0", true), ("echo boom >&2\nexit 1", false)] {
            let mut r = automerge_rig(docker, LABELLED);
            for t in go(&mut r, vec![key(7, "h7")]).await {
                t.await.unwrap();
            }
            assert_eq!(
                calls(&r).contains("-X PUT repos/o/p/pulls/7/merge -f merge_method=squash -f sha=h7"),
                merged,
                "{}",
                calls(&r)
            );
            assert_eq!(r.store.args("ci_merged").len(), usize::from(merged));
            assert_eq!(r.store.args("ci_finish").len(), 1);
        }
    }

    #[tokio::test]
    async fn a_poll_merges_a_green_pull_request_that_asks_late() {
        let mut r = pr_rig("exit 0");
        let bin = r._dir.path().join("bin");
        let log = r._dir.path().join("calls.log");
        let ok = r#"[{"context":"ci/agent/pr/ci","state":"success","description":"agent-ci t: passed in 1m","updated_at":"2026-01-01T00:00:00Z"}]"#;
        script(
            &bin,
            "gh",
            &format!(
                "echo \"gh $*\" >> {l}\nif [ \"$1\" = api ]; then\n case \"$4\" in\n repos/o/p/pulls/7) echo '{LABELLED}';;\n \
                 *commits/*/statuses*) echo '{ok}';;\n *) [ \"$3\" = GET ] && echo '[]';;\n esac\n exit 0\nfi\n\
                 echo '[{{\"number\":7,\"isDraft\":false,\"labels\":[{{\"name\":\"automerge\"}}],\"headRefOid\":\"h7\",\"headRefName\":\"fix\"}}]'",
                l = log.display()
            ),
        );
        r.ci.poll(&r.cfg).await;
        assert!(calls(&r).contains("-X PUT repos/o/p/pulls/7/merge"), "{}", calls(&r));
        assert_eq!(r.store.args("ci_merged").len(), 1);
        assert!(!r.store.args("ci_merge_requests").is_empty());
    }

    #[tokio::test]
    async fn without_a_store_nothing_is_listed_or_run() {
        let mut r = pr_rig("exit 0");
        r.ci.merge = None;
        assert!(r.ci.poll(&r.cfg).await.is_empty());
        let keys = [("o-p".to_string(), vec![key(7, "h7")])].into();
        assert!(r.ci.start(&r.cfg, keys).await.is_empty());
        assert_eq!(calls(&r).matches("docker run").count(), 0);
    }

    fn check_runs(r: &Rig) -> usize {
        calls(r)
            .lines()
            .filter(|l| l.starts_with("docker run") && l.contains("-check "))
            .count()
    }

    #[tokio::test]
    async fn a_failing_check_posts_failure_and_one_comment() {
        let mut r = pr_rig("echo boom; exit 1");
        for t in go(&mut r, vec![key(7, "h7")]).await {
            t.await.unwrap();
        }
        let c = calls(&r);
        assert!(c.contains("state=failure") && !c.contains("state=success"), "{c}");
        assert_eq!(c.matches("issues/7/comments").count(), 1, "{c}"); // the one post
        assert!(c.contains("body=agent-ci t failed on h7."), "{c}");
        assert!(c.contains("boom"), "{c}");
    }

    /// `pr_rig` whose main also holds `.agent-ci/warm.sh`, and whose fetch step prints `fetch_says`.
    fn warm_rig(fetch_says: &str) -> Rig {
        let r = pr_rig("exit 0");
        let work = r._dir.path().join("work");
        run(&work, &["checkout", "-q", "main"]);
        std::fs::write(work.join(".agent-ci/warm.sh"), "true\n").unwrap();
        run(&work, &["add", "."]);
        run(&work, &["commit", "-qm", "warm"]);
        run(&work, &["push", "-q", "origin", "main"]);
        let bin = r._dir.path().join("bin");
        let log = r._dir.path().join("calls.log");
        let stub = docker_stub(&log, "exit 0", "").replace(
            "case \"$n\" in *-check) ;;",
            &format!("case \"$n\" in *-fetch) echo '{fetch_says}';; esac\ncase \"$n\" in *-check) ;;"),
        );
        script(&bin, "docker", &stub);
        r
    }

    fn warm_runs(r: &Rig) -> usize {
        calls(r)
            .lines()
            .filter(|l| l.starts_with("docker run") && l.contains("-warm "))
            .count()
    }

    #[tokio::test]
    async fn warm_runs_only_with_warm_sh_and_the_default_image() {
        let mut r = warm_rig("");
        for t in go(&mut r, vec![key(7, "h7")]).await {
            t.await.unwrap();
        }
        assert_eq!((warm_runs(&r), check_runs(&r)), (1, 1), "{}", calls(&r));
        assert!(
            calls(&r).contains("mcr.microsoft.com/playwright:v1.63.0-noble"),
            "{}",
            calls(&r)
        );
        // The repo's own image: no warm step.
        let mut r = warm_rig("AGENT_CI_IMAGE rust:1");
        for t in go(&mut r, vec![key(7, "h7")]).await {
            t.await.unwrap();
        }
        assert_eq!((warm_runs(&r), check_runs(&r)), (0, 1), "{}", calls(&r));
        // No warm.sh on main: no warm step even on the default image.
        let mut r = pr_rig("exit 0");
        for t in go(&mut r, vec![key(7, "h7")]).await {
            t.await.unwrap();
        }
        assert_eq!(warm_runs(&r), 0);
    }

    #[tokio::test]
    async fn a_bad_image_name_fails_the_check_in_its_log_and_a_push_failure_comments_on_the_commit() {
        let mut r = warm_rig("AGENT_CI_IMAGE bad name");
        let sha = "a".repeat(40);
        for t in go(&mut r, vec![CheckKey::push(&sha)]).await {
            t.await.unwrap();
        }
        let c = calls(&r);
        assert!(c.contains("state=failure"), "{c}");
        assert!(c.contains(&format!("commits/{sha}/comments")), "{c}");
        assert!(c.contains(".agent-ci/image is not an image name: bad name"), "{c}");
        assert_eq!(check_runs(&r), 0, "{c}");
    }

    #[tokio::test]
    async fn one_slot_runs_two_pull_requests_one_after_the_other() {
        let mut r = pr_rig("exit 0");
        let log = r._dir.path().join("calls.log");
        let body = format!("echo begin >> {l}; wait_gate; echo end >> {l}", l = log.display());
        script(&r._dir.path().join("bin"), "docker", &docker_stub(&log, &body, ""));
        let keys = vec![key(7, "h7"), key(8, "h8")];
        let first = go(&mut r, keys.clone()).await;
        assert_eq!(first.len(), 1, "only one permit");
        // Held at the gate, so still running: the second key finds no permit.
        until("the first check to start", || calls(&r).contains("begin")).await;
        assert_eq!(r.ci.running.lock().unwrap().len(), 1);
        assert!(go(&mut r, keys.clone()).await.is_empty());
        open_gate(&r);
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
    async fn recovery_removes_this_computers_leftover_containers() {
        let mut r = pr_rig("exit 0");
        let bin = r._dir.path().join("bin");
        let log = r._dir.path().join("calls.log");
        script(&bin, "docker", &docker_stub(&log, "exit 0", "abc123"));
        r.ci.programs.docker = bin.join("docker").into();
        r.ci.recover(&r.cfg).await;
        let c = calls(&r);
        assert!(c.contains("docker ps -aq --filter name=^agent-ci-t-o-p--"), "{c}");
        assert!(c.contains("docker rm -f abc123"), "{c}");
    }

    #[tokio::test]
    async fn a_shrink_while_a_check_runs_does_not_leave_extra_permits() {
        let mut r = pr_rig("wait_gate");
        let tasks = go(&mut r, vec![key(7, "h7")]).await;
        // Docker "down" for one poll while the check, held at the gate, holds the only permit.
        r.cfg.slots = 0;
        r.ci.poll(&r.cfg).await;
        open_gate(&r);
        for t in tasks {
            t.await.unwrap();
        }
        assert_eq!(r.ci.slot().available_permits(), 0);
        r.cfg.slots = 1;
        r.ci.poll(&r.cfg).await;
        assert_eq!(r.ci.slot().available_permits(), 1);
        assert!(r.ci.running.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_finished_check_does_not_rerun_after_a_poll_that_could_not_read_the_list() {
        let mut r = pr_rig("exit 0");
        r.cfg.slots = 2; // the pull request and the push to main
        let keys = r.ci.poll(&r.cfg).await;
        for t in r.ci.start(&r.cfg, keys).await {
            t.await.unwrap();
        }
        let count = |r: &Rig| (calls(r).matches("state=pending").count(), check_runs(r));
        assert_eq!(count(&r), (2, 2));
        // Still listed: not run again.
        let keys = r.ci.poll(&r.cfg).await;
        assert!(r.ci.start(&r.cfg, keys).await.is_empty());
        // A poll that can't read the list (ciSlots 0), then a healthy one with the same key listed.
        r.cfg.slots = 0;
        r.ci.poll(&r.cfg).await;
        r.cfg.slots = 2;
        let keys = r.ci.poll(&r.cfg).await;
        assert_eq!(keys["o-p"].len(), 2);
        assert!(r.ci.start(&r.cfg, keys).await.is_empty());
        assert_eq!(count(&r), (2, 2));
    }

    #[tokio::test]
    async fn a_head_still_being_checked_against_an_older_main_is_not_started_again_until_that_check_ends() {
        let mut r = pr_rig("exit 0");
        r.cfg.slots = 2;
        let keys = r.ci.poll(&r.cfg).await;
        let pr = keys["o-p"].iter().find(|k| !k.is_push()).unwrap().clone();
        // The same pull request's head, checked against the main before the one just read, is still running.
        let older = CheckKey {
            main: "an-older-main".into(),
            ..pr.clone()
        };
        r.ci.running.lock().unwrap().insert(("o-p".into(), older.clone()));
        let tasks = r.ci.start(&r.cfg, keys).await;
        assert_eq!(tasks.len(), 1, "only the push to main starts");
        for t in tasks {
            t.await.unwrap();
        }
        assert!(
            !r.ci.seen.lock().unwrap().contains(&("o-p".into(), pr.clone())),
            "left for the next poll"
        );
        // That check ends: the next poll starts the new key.
        r.ci.running.lock().unwrap().remove(&("o-p".into(), older));
        let keys = r.ci.poll(&r.cfg).await;
        let tasks = r.ci.start(&r.cfg, keys).await;
        assert_eq!(tasks.len(), 1, "the pull request against the new main");
        for t in tasks {
            t.await.unwrap();
        }
        assert!(r.ci.seen.lock().unwrap().contains(&("o-p".into(), pr)));
    }

    /// A `gh` that keeps commit statuses in `statuses.txt` (`state|description|time`), for claims.
    fn claim_gh(r: &mut Rig, preloaded: &[(&str, &str, i64)]) {
        let root = r._dir.path().to_path_buf();
        let (store, log) = (root.join("statuses.txt"), root.join("calls.log"));
        let lines: String = preloaded
            .iter()
            .map(|(state, desc, at)| {
                let t = chrono::DateTime::from_timestamp_millis(*at)
                    .unwrap()
                    .format("%Y-%m-%dT%H:%M:%SZ");
                format!("{state}|{desc}|{t}\n")
            })
            .collect();
        std::fs::write(&store, lines).unwrap();
        let body = format!(
            "if [ \"$1\" = api ]; then\n echo \"gh $*\" >> {l}\n \
             if [ \"$3\" = POST ]; then for a in \"$@\"; do case \"$a\" in state=*) s=${{a#state=}};; description=*) d=${{a#description=}};; esac; done\n \
             echo \"$s|$d|$(date -u +%Y-%m-%dT%H:%M:%SZ)\" >> {st}; exit 0; fi\n \
             echo '['; sep=; [ -f {st} ] && while IFS='|' read -r s d t; do printf '%s{{\"context\":\"ci/agent/pr/ci\",\"state\":\"%s\",\"description\":\"%s\",\"updated_at\":\"%s\"}}' \"$sep\" \"$s\" \"$d\" \"$t\"; sep=,; done < {st}; echo ']'; exit 0; fi\n\
             echo '[{{\"number\":7,\"isDraft\":false,\"labels\":[],\"headRefOid\":\"h7\"}}]'",
            l = log.display(),
            st = store.display()
        );
        script(&root.join("bin"), "gh", &body);
    }

    const KEY_TEXT: &str = "h7-mmmmmmm";

    fn now() -> i64 {
        crate::telemetry::now_ms()
    }

    async fn run_as(r: &mut Rig, name: &str) {
        r.cfg.computer = name.into();
        r.ci.poll(&r.cfg).await;
        let keys = [("o-p".to_string(), vec![key(7, "h7")])].into();
        for t in r.ci.start(&r.cfg, keys).await {
            t.await.unwrap();
        }
    }

    fn docker_runs(r: &Rig) -> usize {
        check_runs(r)
    }

    #[tokio::test]
    async fn two_loops_sharing_one_store_run_a_key_once() {
        let mut a = pr_rig("wait_gate");
        claim_gh(&mut a, &[]);
        // The store gives the lease to the first claim and names its holder to the next.
        let taken = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let store = FakeStore::answering(move |op, _| {
            Ok(match op {
                "ci_claim" if taken.swap(true, Ordering::SeqCst) => {
                    json!({ "won": false, "holder": { "computer": "a" } })
                }
                "ci_claim" => json!({ "won": true }),
                "ci_merge_requests" => json!({ "requests": [] }),
                _ => json!({ "ok": true }),
            })
        });
        a.ci = std::mem::replace(&mut a.ci, CiLoop::new(Programs::default())).with_store(store.clone());
        let mut b = CiLoop::new(a.ci.programs.clone()).with_store(store.clone());
        b.remote = a.ci.remote.clone();
        let (mut cfg_a, mut cfg_b) = (a.cfg.clone(), a.cfg.clone());
        (cfg_a.computer, cfg_b.computer) = ("a".into(), "b".into());
        let keys: BTreeMap<_, _> = [("o-p".to_string(), vec![key(7, "h7")])].into();
        a.ci.poll(&cfg_a).await;
        b.poll(&cfg_b).await;
        let ta = a.ci.start(&cfg_a, keys.clone()).await;
        until("a's claim", || calls(&a).contains("state=pending")).await;
        // b asks while a's check is held at the gate, and loses.
        for t in b.start(&cfg_b, keys).await {
            t.await.unwrap();
        }
        open_gate(&a);
        for t in ta {
            t.await.unwrap();
        }
        assert_eq!(docker_runs(&a), 1, "{}", calls(&a));
        assert_eq!(store.args("ci_claim").len(), 2);
        // The loser forgets the key so it looks again; the winner's finished key is not run again.
        assert!(b.seen.lock().unwrap().is_empty());
        assert_eq!(a.ci.seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn only_a_won_lease_runs_the_check() {
        for (answer, runs, done) in [
            (json!({ "won": true }), 1, true),
            (json!({ "won": false, "holder": { "computer": "z" } }), 0, false),
            (json!({ "won": false, "finished": true }), 0, true),
        ] {
            let mut r = pr_rig("exit 0");
            claim_gh(&mut r, &[]);
            let store = FakeStore::answering(move |op, _| {
                Ok(if op == "ci_claim" {
                    answer.clone()
                } else {
                    json!({ "ok": true })
                })
            });
            r.ci = std::mem::replace(&mut r.ci, CiLoop::new(Programs::default())).with_store(store);
            run_as(&mut r, "a").await;
            assert_eq!(docker_runs(&r), runs);
            assert_eq!(r.ci.seen.lock().unwrap().len(), usize::from(done));
        }
        // A store that can't be reached: nothing runs, and the key is tried again.
        let mut r = pr_rig("exit 0");
        claim_gh(&mut r, &[]);
        let down = FakeStore::answering(|_, _| Err("down".into()));
        r.ci = std::mem::replace(&mut r.ci, CiLoop::new(Programs::default())).with_store(down);
        run_as(&mut r, "a").await;
        assert_eq!(docker_runs(&r), 0);
        assert!(r.ci.seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn another_computers_pending_status_is_left_to_the_lease() {
        let desc = format!("agent-ci z: running ({KEY_TEXT})");
        let mut r = pr_rig("exit 0");
        claim_gh(&mut r, &[("pending", &desc, now() - 14 * 60_000)]);
        run_as(&mut r, "a").await;
        // The status's age doesn't matter: the store's answer (won) is what lets it run.
        assert_eq!(docker_runs(&r), 1, "{}", calls(&r));
        assert_eq!(r.store.args("ci_claim").len(), 1);
    }

    #[tokio::test]
    async fn a_finished_key_is_never_rerun() {
        for state in ["success", "failure"] {
            let mut r = pr_rig("exit 0");
            claim_gh(
                &mut r,
                &[(state, &format!("agent-ci z: done ({KEY_TEXT})"), now() - 60_000)],
            );
            run_as(&mut r, "a").await;
            assert_eq!(docker_runs(&r), 0);
            assert!(!calls(&r).contains("-X POST"));
            assert!(r.store.args("ci_claim").is_empty());
        }
    }

    #[tokio::test]
    async fn a_long_run_refreshes_its_claim() {
        let mut r = pr_rig("wait_gate");
        claim_gh(&mut r, &[]);
        r.ci.refresh = Duration::from_millis(20);
        // The check is held at the gate until the claim has been refreshed three times.
        let (log, gate) = (r._dir.path().join("calls.log"), r._dir.path().join("gate"));
        let opener = tokio::spawn(async move {
            until("three refreshes", || {
                std::fs::read_to_string(&log)
                    .unwrap_or_default()
                    .matches("state=pending")
                    .count()
                    >= 4
            })
            .await;
            std::fs::write(gate, "").unwrap();
        });
        run_as(&mut r, "a").await;
        opener.await.unwrap();
        // The claim, then a refresh each interval of the run, then success.
        assert!(calls(&r).matches("state=pending").count() >= 4, "{}", calls(&r));
        assert_eq!(calls(&r).matches("state=success").count(), 1);
        assert!(r.store.args("ci_heartbeat").len() >= 2);
        assert_eq!(r.store.args("ci_heartbeat")[0]["context"], "ci/agent/pr/ci");
    }

    #[tokio::test]
    async fn the_final_status_is_the_last_even_when_posting_it_is_slow() {
        let mut r = pr_rig("exit 0");
        claim_gh(&mut r, &[]);
        // A `gh` that takes a while to post the final status, long enough for a refresh to come due.
        let bin = r._dir.path().join("bin");
        let body = std::fs::read_to_string(bin.join("gh")).unwrap();
        let slow = body.replacen(
            "if [ \"$3\" = POST ]; then ",
            "if [ \"$3\" = POST ]; then case \"$*\" in *state=success*|*state=failure*) sleep 1;; esac; ",
            1,
        );
        script(&bin, "gh", slow.split_once('\n').unwrap().1);
        r.ci.refresh = Duration::from_millis(200);
        run_as(&mut r, "a").await;
        let store = std::fs::read_to_string(r._dir.path().join("statuses.txt")).unwrap();
        assert!(store.lines().last().unwrap().starts_with("success|"), "{store}");
    }

    async fn poll_and_start(r: &mut Rig) -> Vec<tokio::task::JoinHandle<()>> {
        let keys = r.ci.poll(&r.cfg).await;
        r.ci.start(&r.cfg, keys).await
    }

    /// `pr_rig` with no pull request ready, so only main's own push run is listed.
    fn push_rig(docker_body: &str) -> Rig {
        let r = pr_rig(docker_body);
        let bin = r._dir.path().join("bin");
        let log = r._dir.path().join("calls.log");
        script(
            &bin,
            "gh",
            &format!(
                "if [ \"$1\" = api ]; then echo \"gh $*\" >> {l}; [ \"$3\" = GET ] && echo '[]'; exit 0; fi
echo '[]'",
                l = log.display()
            ),
        );
        r
    }

    #[tokio::test]
    async fn a_new_main_sha_gets_one_push_run_with_statuses_on_main_and_no_comment() {
        let mut r = push_rig("exit 0");
        let main = Command::new("git")
            .args(["rev-parse", "origin/main"])
            .current_dir(r._dir.path().join("work"))
            .output()
            .unwrap();
        let main = String::from_utf8(main.stdout).unwrap().trim().to_string();
        for t in poll_and_start(&mut r).await {
            t.await.unwrap();
        }
        let c = calls(&r);
        assert!(c.contains("bash .agent-ci/checks.sh"), "{c}");
        assert!(c.contains(&format!("statuses/{main}")), "{c}");
        assert!(
            c.contains("context=ci/agent/push/ci") && !c.contains("ci/agent/pr/ci"),
            "{c}"
        );
        assert!(c.contains("state=pending") && c.contains("state=success"), "{c}");
        assert!(!c.contains("/comments"), "{c}");
        assert!(events(&r).contains("push main"));
        // The same main sha isn't run again.
        assert!(poll_and_start(&mut r).await.is_empty());
        assert_eq!(docker_runs(&r), 1);
    }

    #[tokio::test]
    async fn a_failing_push_run_posts_failure_on_main_and_a_commit_comment() {
        let mut r = push_rig("echo boom; exit 1");
        for t in poll_and_start(&mut r).await {
            t.await.unwrap();
        }
        let c = calls(&r);
        assert!(
            c.contains("state=failure") && c.contains("/commits/") && c.contains("/comments"),
            "{c}"
        );
        assert!(!c.contains("issues/"), "{c}");
    }

    #[tokio::test]
    async fn a_push_run_takes_a_permit_like_a_pull_request_check() {
        let mut r = pr_rig("wait_gate");
        // One slot, a pull request and the push: the push waits for the permit.
        let tasks = poll_and_start(&mut r).await;
        assert_eq!(tasks.len(), 1);
        assert_eq!(r.ci.slot().available_permits(), 0);
        open_gate(&r);
        for t in tasks {
            t.await.unwrap();
        }
        assert_eq!(poll_and_start(&mut r).await.len(), 1);
    }
}
