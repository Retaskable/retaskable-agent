use super::*;
use crate::backlog::pool::contract::{main_facts, main_result};
use crate::config::ProjectConfig;
use crate::test_support::{git, repo_with_origin, run as run_git, write_executable};
use serde_json::json;

fn sha(c: char) -> String {
    c.to_string().repeat(40)
}

fn seen(branch: &str, head: &str) -> Seen {
    Seen {
        branch: branch.into(),
        sha: head.into(),
        tree: Some("t".repeat(40)),
        behind: None,
        rebase: None,
    }
}

fn run(fact: &MainFacts) -> serde_json::Value {
    let v = serde_json::to_value(fact).unwrap();
    main_facts(&v).unwrap_or_else(|e| panic!("the store refuses {v}: {e}"));
    v
}

#[test]
fn nothing_is_said_before_the_first_fetch_and_the_facts_fit_what_the_store_takes() {
    let held = MainLook::default();
    assert_eq!(held.facts(false), None);
    held.see(Seen {
        behind: Some(Behind {
            branch: "main".into(),
            head: sha('b'),
            count: 3,
        }),
        ..seen("dev/x", &sha('a'))
    });
    let v = run(&held.facts(false).unwrap());
    assert_eq!(v["branch"], "dev/x");
    assert_eq!(v["behind"], json!({ "branch": "main", "head": sha('b'), "count": 3 }));
    assert!(v.get("busy").is_none() && v.get("baseline").is_none() && v.get("ci").is_none());
    assert_eq!(run(&held.facts(true).unwrap())["busy"], true);
}

#[test]
fn a_tree_that_is_not_there_is_null_and_the_coverage_key_follows_it() {
    let held = MainLook::default();
    held.see(Seen {
        tree: None,
        ..seen("main", &sha('a'))
    });
    held.hold_coverage("main", None);
    let v = run(&held.facts(false).unwrap());
    assert_eq!(v["tree"], serde_json::Value::Null);
    assert_eq!(v["coverage"], json!({ "base": "main", "tree": null }));
}

#[test]
fn what_would_fail_the_whole_health_is_left_out_not_sent() {
    let held = MainLook::default();
    held.see(Seen {
        behind: Some(Behind {
            branch: "main".into(),
            head: "not-a-commit".into(),
            count: 1,
        }),
        ..seen("main", &sha('a'))
    });
    assert_eq!(held.facts(false).unwrap().behind, None, "a head that is no commit");
    held.see(seen("main", "xyz"));
    assert_eq!(held.facts(false), None, "a head that is no commit is no fact at all");
    held.see(seen("", &sha('a')));
    assert_eq!(held.facts(false), None);
    held.see(seen("main", &sha('a')));
    let mut ci = MainCi::of(
        "short",
        Ok(crate::ci_checks::CommitChecks {
            state: crate::ci_checks::CommitState::Pass,
            failed: vec![],
        }),
    );
    held.hold_ci(&ci);
    assert_eq!(held.facts(false).unwrap().ci, None, "a result for a sha too short");
    ci.sha = sha('a');
    held.hold_ci(&ci);
    assert_eq!(
        run(&held.facts(false).unwrap())["ci"],
        json!({ "sha": sha('a'), "state": "pass", "failed": [] })
    );
}

fn check_run(name: &str, ok: bool) -> CheckRun {
    CheckRun {
        name: name.into(),
        command: "x".into(),
        blocking: true,
        exit_code: i32::from(!ok),
        timed_out: false,
        output_tail: String::new(),
        preexisting: false,
        seconds: 1.0,
    }
}

#[test]
fn names_are_whole_short_and_few() {
    let held = MainLook::default();
    held.see(seen("main", &sha('a')));
    let mut runs: Vec<CheckRun> = (0..80).map(|i| check_run(&format!("check {i}"), false)).collect();
    runs.push(check_run(&"é".repeat(400), false));
    runs.push(check_run("  ", false));
    runs[0].blocking = false;
    let fact = held.hold_baseline(&sha('a'), &runs);
    assert_eq!(fact.failed.len(), 50);
    assert_eq!(
        fact.failed[0], "check 1",
        "a check that does not block is no failure of the baseline"
    );
    run(&held.facts(false).unwrap());
    let long = held.hold_baseline(&sha('a'), &[check_run(&"é".repeat(400), false), check_run("", false)]);
    assert_eq!(long.failed.len(), 1);
    assert_eq!(long.failed[0].chars().count(), 300);
}

#[test]
fn a_failure_that_names_no_check_is_none_and_a_cancelled_run_is_no_failure() {
    use crate::ci_checks::{CommitChecks, CommitState, FailedCheck};
    let failed = |name: &str, cancelled| FailedCheck {
        name: name.into(),
        link: String::new(),
        cancelled,
        completed_at: None,
    };
    let of = |f: Vec<FailedCheck>| {
        ci_fact(&MainCi::of(
            &sha('a'),
            Ok(CommitChecks {
                state: CommitState::Fail,
                failed: f,
            }),
        ))
    };
    assert_eq!(of(vec![failed("lint", true)]).state, "none");
    assert_eq!(of(vec![failed(" ", false)]).state, "none");
    let red = of(vec![failed("test", false), failed("lint", true)]);
    assert_eq!((red.state, red.failed), ("fail", vec!["test".to_string()]));
}

/// A project on GitHub whose base branch is `main` of a real repo with an origin, with the given checks.
struct Fixture {
    root: tempfile::TempDir,
    repo: PathBuf,
    cfg: ProjectConfig,
    id: String,
    held: MainLook,
    facts: KeptFacts,
    head: String,
}

fn fixture(checks: serde_json::Value) -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let repo = repo_with_origin(root.path());
    let id = format!("ml-{}", root.path().file_name().unwrap().to_string_lossy());
    let cfg: ProjectConfig = serde_json::from_value(json!({
        "repoPath": repo.display().to_string(),
        "githubRepo": "o/r",
        "baseBranch": "main",
        "worktreesDir": root.path().join("wt").display().to_string(),
        "checks": checks,
    }))
    .unwrap();
    let head = crate::lanes::head_sha(&repo).unwrap();
    Fixture {
        root,
        repo,
        cfg,
        id,
        held: MainLook::default(),
        facts: KeptFacts::default(),
        head,
    }
}

fn spec(want: &str, sha: &str) -> JobSpec {
    JobSpec {
        kind: "main".into(),
        day: "2026-11-17".into(),
        args: json!({ "want": want, "sha": sha }).as_object().unwrap().clone(),
    }
}

/// Runs the job and answers its result, which must pass the store's own check.
async fn go(f: &Fixture, gh: &Path, want: &str, sha: &str) -> JobOutcome {
    let look = Look {
        id: &f.id,
        project: &f.cfg,
        held: &f.held,
        facts: &f.facts,
        gh,
    };
    let out = main_job(&look, &spec(want, sha)).await;
    if let Outcome::Done { result, .. } = &out {
        main_result(result).unwrap_or_else(|e| panic!("the store refuses {result}: {e}"));
    }
    out
}

fn result(out: JobOutcome) -> serde_json::Value {
    match out {
        Outcome::Done { result, .. } => result,
        other => panic!("{other:?}"),
    }
}

/// A fake `gh` that answers the commit's statuses (none) and check runs with `runs`, one object a line.
fn gh_with_runs(root: &Path, runs: &[serde_json::Value]) -> PathBuf {
    let path = root.join("gh");
    let lines: Vec<String> = runs.iter().map(|r| r.to_string()).collect();
    let script = format!(
        "#!/bin/sh\ncase \"$*\" in *check-runs*) cat <<'EOF'\n{}\nEOF\n;; esac\n",
        lines.join("\n")
    );
    write_executable(&path, &script);
    path
}

fn run_of(name: &str, conclusion: &str) -> serde_json::Value {
    json!({ "name": name, "status": "completed", "conclusion": conclusion, "html_url": format!("https://github.com/o/r/actions/runs/1/job/{name}"), "completed_at": "2026-11-17T10:00:00Z" })
}

#[tokio::test]
async fn a_commit_main_has_moved_past_is_answered_with_the_bare_sha_and_nothing_is_looked_at() {
    let f = fixture(json!([]));
    let gh = gh_with_runs(f.root.path(), &[run_of("test", "failure")]);
    for want in ["ci", "coverage", "baseline"] {
        let r = result(go(&f, &gh, want, &sha('1')).await);
        assert_eq!(r, json!({ "sha": sha('1') }), "{want}");
    }
    assert!(f.facts.owing().is_empty(), "nothing was written: {:?}", f.facts.owing());
    assert_eq!(f.held.facts(false), None);
}

#[tokio::test]
async fn a_look_this_computer_does_not_know_is_refused_in_words() {
    let f = fixture(json!([]));
    let gh = gh_with_runs(f.root.path(), &[]);
    assert!(matches!(go(&f, &gh, "teleport", &f.head).await, Outcome::Refused(why) if why.contains("doesn't know")));
    let look = Look {
        id: &f.id,
        project: &f.cfg,
        held: &f.held,
        facts: &f.facts,
        gh: &gh,
    };
    let no_sha = JobSpec {
        args: json!({ "want": "ci" }).as_object().unwrap().clone(),
        ..spec("ci", "")
    };
    assert!(matches!(main_job(&look, &no_sha).await, Outcome::Refused(_)));
}

#[tokio::test]
async fn a_ci_look_reads_the_host_keeps_the_fact_and_answers_the_failed_checks_with_a_brief() {
    let f = fixture(json!([]));
    let gh = gh_with_runs(
        f.root.path(),
        &[
            run_of("test", "failure"),
            run_of("lint", "cancelled"),
            run_of("build", "success"),
        ],
    );
    let r = result(go(&f, &gh, "ci", &f.head).await);
    assert_eq!(r["ci"], json!({ "sha": f.head, "state": "fail", "failed": ["test"] }));
    let brief = r["brief"].as_str().unwrap();
    assert!(
        brief.contains("test") && brief.contains("https://github.com/o/r/actions/runs/1/job/test"),
        "{brief}"
    );
    assert!(brief.chars().count() <= BRIEF_MAX);
    assert_eq!(
        f.facts.owed(MAIN_CI).unwrap()["state"],
        "fail",
        "kept as the store's fact too"
    );

    let ok = gh_with_runs(f.root.path(), &[run_of("build", "success")]);
    let r = result(go(&f, &ok, "ci", &f.head).await);
    assert_eq!(r["ci"]["state"], "pass");
    assert!(r.get("brief").is_none(), "nothing failed, no brief");

    let cancelled = gh_with_runs(f.root.path(), &[run_of("lint", "cancelled")]);
    let r = result(go(&f, &cancelled, "ci", &f.head).await);
    assert_eq!(
        r["ci"],
        json!({ "sha": f.head, "state": "none", "failed": [] }),
        "a cancelled run is no failure of main's"
    );
}

#[tokio::test]
async fn a_ci_look_is_refused_where_the_host_cannot_say() {
    let mut f = fixture(json!([]));
    f.cfg.code_host = serde_json::from_value(json!({ "kind": "gitlab" })).unwrap();
    let gh = gh_with_runs(f.root.path(), &[]);
    assert!(matches!(go(&f, &gh, "ci", &f.head).await, Outcome::Refused(why) if why.contains("can't say")));
}

#[tokio::test]
async fn a_coverage_look_writes_the_fact_and_holds_its_key_and_another_host_holds_the_key_without_a_row() {
    let f = fixture(json!([]));
    let gh = gh_with_runs(f.root.path(), &[]);
    result(go(&f, &gh, "coverage", &f.head).await);
    let fact = f.facts.owed(CI_COVERAGE).unwrap();
    assert_eq!(fact["base"], "main");
    f.held.see(seen("main", &f.head));
    assert_eq!(
        f.held.facts(false).unwrap().coverage,
        Some(CoverageKey {
            base: "main".into(),
            tree: None
        })
    );

    let g = fixture(json!([]));
    let mut g = Fixture {
        cfg: ProjectConfig {
            code_host: serde_json::from_value(json!({ "kind": "gitlab" })).unwrap(),
            ..g.cfg.clone()
        },
        ..g
    };
    g.cfg.github_repo = String::new();
    result(go(&g, &gh, "coverage", &g.head).await);
    assert!(g.facts.owed(CI_COVERAGE).is_none(), "no row rather than a wrong none");
    g.held.see(seen("main", &g.head));
    assert!(
        g.held.facts(false).unwrap().coverage.is_some(),
        "so the store stops asking"
    );
}

fn push_commit(repo: &Path) -> String {
    run_git(git(repo).args(["commit", "--quiet", "--allow-empty", "-m", "more"]));
    run_git(git(repo).args(["push", "--quiet", "origin", "main"]));
    crate::lanes::head_sha(repo).unwrap()
}

#[tokio::test]
async fn a_baseline_look_runs_the_checks_in_the_base_checkout_and_answers_what_failed_with_its_output() {
    let f = fixture(
        json!([{ "name": "unit", "command": "echo broke-here && exit 1", "blocking": true, "timeoutSec": 30 }, { "name": "lint", "command": "exit 0", "blocking": true, "timeoutSec": 30 }]),
    );
    let gh = gh_with_runs(f.root.path(), &[]);
    let r = result(go(&f, &gh, "baseline", &f.head).await);
    assert_eq!(r["baseline"], json!({ "sha": f.head, "failed": ["unit"] }));
    let brief = r["brief"].as_str().unwrap();
    assert!(brief.contains("unit") && brief.contains("broke-here"), "{brief}");
    let kept = f.facts.owed(BASELINE).unwrap();
    assert_eq!(kept["results"][0]["ok"], false);
    assert!(!kept.to_string().contains("broke-here"), "a fact holds no output");

    // Main moves on: the same look for the old commit is over.
    let next = push_commit(&f.repo);
    assert_ne!(next, f.head);
    assert_eq!(result(go(&f, &gh, "baseline", &f.head).await), json!({ "sha": f.head }));
}

#[tokio::test]
async fn a_baseline_with_no_approved_checks_is_refused_not_a_green_main() {
    let f = fixture(json!([]));
    let gh = gh_with_runs(f.root.path(), &[]);
    assert!(matches!(go(&f, &gh, "baseline", &f.head).await, Outcome::Refused(why) if why.contains("approved")));
    assert_eq!(f.held.facts(false), None);
}

#[tokio::test]
async fn a_green_baseline_has_no_brief_and_a_taken_checkout_answers_busy() {
    let f = fixture(json!([{ "name": "unit", "command": "exit 0", "blocking": true, "timeoutSec": 30 }]));
    let gh = gh_with_runs(f.root.path(), &[]);
    let hold = crate::base_hold::hold_base(&f.id, "a play").unwrap();
    assert_eq!(
        result(go(&f, &gh, "baseline", &f.head).await),
        json!({ "sha": f.head, "busy": true })
    );
    drop(hold);
    let r = result(go(&f, &gh, "baseline", &f.head).await);
    assert_eq!(r, json!({ "sha": f.head, "baseline": { "sha": f.head, "failed": [] } }));
}

#[tokio::test]
async fn a_base_branch_that_is_not_there_has_nothing_to_look_at() {
    let mut f = fixture(json!([{ "name": "unit", "command": "exit 0", "blocking": true, "timeoutSec": 30 }]));
    f.cfg.base_branch = "no-such-branch".into();
    let gh = gh_with_runs(f.root.path(), &[]);
    // The head of a branch that is not there is nothing for the store to ask about.
    assert_eq!(result(go(&f, &gh, "baseline", &f.head).await), json!({ "sha": f.head }));
}

fn commit_file(repo: &Path, name: &str, text: &str) {
    std::fs::write(repo.join(name), text).unwrap();
    run_git(git(repo).args(["add", name]));
    run_git(git(repo).args(["commit", "--quiet", "-m", name]));
}

fn remote_sha(repo: &Path, branch: &str) -> String {
    let out = git(repo)
        .args(["ls-remote", "origin", &format!("refs/heads/{branch}")])
        .output()
        .unwrap();
    String::from_utf8(out.stdout)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string()
}

/// An integration-branch project: `dev` is the base, one commit of its own, and `main` has moved one commit ahead.
fn integration(conflict: bool) -> (Fixture, String) {
    let root = tempfile::tempdir().unwrap();
    let repo = repo_with_origin(root.path());
    run_git(git(&repo).args(["checkout", "--quiet", "-b", "dev"]));
    commit_file(&repo, "c.txt", "dev side");
    run_git(git(&repo).args(["push", "--quiet", "origin", "dev"]));
    run_git(git(&repo).args(["checkout", "--quiet", "main"]));
    commit_file(&repo, if conflict { "c.txt" } else { "m.txt" }, "main side");
    run_git(git(&repo).args(["push", "--quiet", "origin", "main"]));
    let cfg: ProjectConfig = serde_json::from_value(json!({
        "repoPath": repo.display().to_string(), "githubRepo": "o/r", "baseBranch": "dev",
        "worktreesDir": root.path().join("wt").display().to_string(),
        "github": { "pattern": "integration" },
    }))
    .unwrap();
    let id = format!("rb-{}", root.path().file_name().unwrap().to_string_lossy());
    let head = remote_sha(&repo, "dev");
    let main = remote_sha(&repo, "main");
    (
        Fixture {
            root,
            repo,
            cfg,
            id,
            held: MainLook::default(),
            facts: KeptFacts::default(),
            head,
        },
        main,
    )
}

async fn rebase_go(f: &Fixture, sha: &str, main: &str, main_sha: &str) -> JobOutcome {
    let gh = f.root.path().join("gh");
    let look = Look {
        id: &f.id,
        project: &f.cfg,
        held: &f.held,
        facts: &f.facts,
        gh: &gh,
    };
    let mut s = spec("rebase", sha);
    s.args.insert("main".into(), json!(main));
    s.args.insert("mainSha".into(), json!(main_sha));
    let out = main_job(&look, &s).await;
    if let Outcome::Done { result, .. } = &out {
        main_result(result).unwrap_or_else(|e| panic!("the store refuses {result}: {e}"));
    }
    out
}

#[tokio::test]
async fn a_rebase_look_rebases_with_the_lease_and_answers_an_empty_rebase() {
    let (f, main) = integration(false);
    let r = result(rebase_go(&f, &f.head, "main", &main).await);
    assert_eq!(r, json!({ "sha": f.head, "rebase": {} }));
    assert_ne!(remote_sha(&f.repo, "dev"), f.head, "pushed");
}

#[tokio::test]
async fn a_conflicting_rebase_answers_the_files_and_pushes_nothing() {
    let (f, main) = integration(true);
    let r = result(rebase_go(&f, &f.head, "main", &main).await);
    assert_eq!(r, json!({ "sha": f.head, "rebase": { "conflict": ["c.txt"] } }));
    assert_eq!(remote_sha(&f.repo, "dev"), f.head);
}

#[tokio::test]
async fn a_rebase_whose_push_is_refused_answers_the_error() {
    let (f, main) = integration(false);
    // A push hook that refuses stands in for a branch that cannot be pushed.
    let hooks = f.root.path().join("origin.git").join("hooks");
    if hooks.exists() {
        write_executable(&hooks.join("pre-receive"), "#!/bin/sh\necho no >&2\nexit 1\n");
        let r = result(rebase_go(&f, &f.head, "main", &main).await);
        let why = r["rebase"]["error"].as_str().unwrap().to_string();
        assert!(why.starts_with("Couldn't push dev rebased onto main:"), "{why}");
        assert_eq!(remote_sha(&f.repo, "dev"), f.head);
    }
}

#[tokio::test]
async fn a_rebase_look_for_heads_that_moved_on_answers_the_bare_sha() {
    let (f, main) = integration(false);
    // main moved since the store asked
    assert_eq!(
        result(rebase_go(&f, &f.head, "main", &sha('9')).await),
        json!({ "sha": f.head })
    );
    // the base branch moved since the store asked
    assert_eq!(
        result(rebase_go(&f, &sha('8'), "main", &main).await),
        json!({ "sha": sha('8') })
    );
    assert_eq!(remote_sha(&f.repo, "dev"), f.head, "nothing was rebased");
}

#[tokio::test]
async fn a_rebase_look_is_busy_while_anything_waits_to_go_in_or_the_checkout_is_taken() {
    let (f, main) = integration(false);
    f.held.set_waiting(vec!["a-1".into()]);
    assert_eq!(
        result(rebase_go(&f, &f.head, "main", &main).await),
        json!({ "sha": f.head, "busy": true })
    );
    f.held.set_waiting(vec![]);
    let hold = crate::base_hold::hold_base(&f.id, "a play").unwrap();
    assert_eq!(
        result(rebase_go(&f, &f.head, "main", &main).await),
        json!({ "sha": f.head, "busy": true })
    );
    drop(hold);
    assert_eq!(remote_sha(&f.repo, "dev"), f.head, "nothing was rebased while busy");
    assert_eq!(
        result(rebase_go(&f, &f.head, "main", &main).await),
        json!({ "sha": f.head, "rebase": {} })
    );
}

#[tokio::test]
async fn a_rebase_look_is_refused_where_the_project_does_not_catch_up_by_a_rebase() {
    let (mut f, main) = integration(false);
    f.cfg.github = None;
    assert!(matches!(rebase_go(&f, &f.head, "main", &main).await, Outcome::Refused(why) if why.contains("rebase")));
}

#[test]
fn the_rebase_fact_carries_both_heads_and_what_waits_read_when_it_is_made() {
    let held = MainLook::default();
    held.see(Seen {
        rebase: Some(Rebase {
            branch: "main".into(),
            head: sha('b'),
            base: sha('a'),
            count: 2,
            waiting: vec![],
        }),
        ..seen("dev", &sha('a'))
    });
    assert!(
        run(&held.facts(false).unwrap())["rebase"]["waiting"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    held.set_waiting(vec!["a-1".into(), "".into(), "d-2".into()]);
    let v = run(&held.facts(false).unwrap());
    assert_eq!(
        v["rebase"],
        json!({ "branch": "main", "head": sha('b'), "base": sha('a'), "count": 2, "waiting": ["a-1", "d-2"] })
    );
    assert!(v.get("behind").is_none(), "a rebase pattern reports no behind");
    held.see(Seen {
        rebase: Some(Rebase {
            branch: "main".into(),
            head: "zz".into(),
            base: sha('a'),
            count: 2,
            waiting: vec![],
        }),
        ..seen("dev", &sha('a'))
    });
    assert!(
        held.facts(false).unwrap().rebase.is_none(),
        "a head that is no commit is left out"
    );
}
