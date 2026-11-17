use serde_json::json;

use super::*;
use crate::engines::engine::{test_config, test_result};

fn job(args: Value) -> JobSpec {
    JobSpec::from_json(&json!({ "kind": "triage", "day": "2026-11-17", "args": args })).unwrap()
}

fn args() -> Value {
    json!({
        "id": "login-form", "title": "Login form", "slice": 2, "specRefs": ["docs/specs/login.md", "docs/specs/auth.md"],
        "branch": "retaskable/login-form", "prUrl": "https://github.com/o/r/pull/9",
        "failedIn": "reviewing", "reason": "CI stayed red", "triages": 1, "maxTriages": 3,
        "timeline": [{ "at": "10:00", "text": "Reviewer asked for changes" }, { "at": "11:00" }, { "text": "no time" }],
        "checks": [
            { "ok": true, "name": "lint", "blocking": true, "preexisting": false, "durationSec": 3 },
            { "ok": false, "name": "e2e", "blocking": false, "preexisting": true, "durationSec": 12.5 }
        ]
    })
}

struct Fixture {
    cfg: EphemeralSessionConfig<'static>,
    bugs: Vec<KnownBug>,
}

fn fixture() -> Fixture {
    Fixture {
        cfg: test_config(),
        bugs: vec![KnownBug {
            id: "item-1".into(),
            title: "Prompt  var\nmissing".into(),
        }],
    }
}

fn env<'a>(f: &'a Fixture, kind: &'a str, limit: &'a dyn Fn() -> i64) -> TriageEnv<'a> {
    TriageEnv {
        has_checkout: true,
        base_branch: "main",
        code_host_kind: kind,
        branches_here: false,
        runs_dir: "/data/runs/login-form",
        known_bugs: &f.bugs,
        session: &f.cfg,
        limit_resume_ms: limit,
    }
}

fn at_ten_minutes() -> i64 {
    600_000
}

#[test]
fn the_args_are_read_as_the_task_sends_them() {
    let a = TriageArgs::from_job(&job(args()));
    assert_eq!(
        (a.id.as_str(), a.slice, a.triages, a.max_triages, a.check),
        ("login-form", 2, 1, 3, false)
    );
    assert_eq!(a.spec_refs, ["docs/specs/login.md", "docs/specs/auth.md"]);
    assert_eq!(a.timeline.len(), 2, "a line with no text is dropped");
    assert_eq!(
        a.timeline[1].at, "undefined",
        "as the TypeScript agent prints a missing time"
    );
    assert_eq!((a.checks[1].ok, a.checks[1].duration_sec.as_str()), (false, "12.5"));
    let bare = TriageArgs::from_job(&job(
        json!({ "look": "check", "first": { "suggest": "start_over" }, "branch": "", "triages": "x" }),
    ));
    assert_eq!(
        (bare.check, bare.branch, bare.max_triages, bare.triages),
        (true, None, 2, 0)
    );
    assert_eq!(bare.first.suggest, "start_over");
    assert_eq!(
        TriageArgs::from_job(&job(json!({}))),
        TriageArgs {
            max_triages: 2,
            ..Default::default()
        }
    );
}

#[test]
fn the_code_hosts_words() {
    assert_eq!(host_words("", false), host_words("github", true));
    assert_eq!(
        (host_words("github", false).pr, host_words("github", false).host_edits),
        ("PR", ", no `gh pr` edits")
    );
    assert_eq!(host_words("gitlab", false).pr, "merge request");
    assert!(host_words("gitlab", false).host_reads.contains("`glab mr view`"));
    assert_eq!(
        host_words("remote", true).host_reads,
        "the change is its branch, kept on this computer"
    );
    assert_eq!(
        host_words("remote", false).host_reads,
        "the change is its branch, pushed to origin"
    );
    assert_eq!(
        (host_words("local", false).pr, host_words("local", false).host_edits),
        ("change", "")
    );
}

#[test]
fn known_bugs_and_checks_are_listed_in_the_prompts_words() {
    let none: [KnownBug; 0] = [];
    assert_eq!(known_bugs_list(&none), "(none)");
    assert_eq!(known_bugs_list(&fixture().bugs), "- item-1: Prompt var missing");
    let a = TriageArgs::from_job(&job(args()));
    assert_eq!(
        checks_summary(&a.checks, "PR"),
        "✓ lint 3s\n✗ e2e (non-blocking) (fails on main too, not this PR's fault) 12.5s"
    );
}

#[test]
fn the_first_look_prompt_names_the_item_the_work_and_the_hosts_words() {
    let f = fixture();
    let a = TriageArgs::from_job(&job(args()));
    let p = triage_prompt(&a, &env(&f, "github", &at_ten_minutes)).unwrap();
    assert!(p.contains("- **Item login-form: Login form**, slice 2"));
    assert!(p.contains("- Specs: docs/specs/login.md, docs/specs/auth.md\n"));
    assert!(p.contains("- Branch: retaskable/login-form; PR: https://github.com/o/r/pull/9"));
    assert!(p.contains("- Its work is on the PR's branch."));
    assert!(p.contains("It stopped while `reviewing`, because:\n\nCI stayed red"));
    assert!(p.contains("Automatic decisions already made for this slice: 1 of 3"));
    assert!(p.contains("- 10:00 Reviewer asked for changes\n- undefined no time\n\n## Last check results\n\n✓ lint 3s\n✗ e2e (non-blocking) (fails on main too, not this PR's fault) 12.5s"));
    assert!(p.contains("You're in a checkout of the base branch `main`."));
    assert!(p.contains("in `/data/runs/login-form`."));
    assert!(p.contains("- `gh pr view` and `gh pr checks` on the PR, `git log` and `git diff origin/main...HEAD`"));
    assert!(p.contains("no git commits, pushes or checkouts, no `gh pr` edits)"));
    assert!(p.contains("- item-1: Prompt var missing"));
    assert!(!p.contains("{{"), "every variable filled, in the variables too");
    let gl = triage_prompt(&a, &env(&f, "gitlab", &at_ten_minutes)).unwrap();
    assert!(gl.contains("- Its work is on the merge request's branch."));
}

#[test]
fn the_work_line_depends_on_what_the_item_has() {
    let f = fixture();
    let mut a = TriageArgs::from_job(&job(args()));
    a.pr_url = None;
    let p = triage_prompt(&a, &env(&f, "github", &at_ten_minutes)).unwrap();
    assert!(p.contains("- It has no PR yet; its work so far is pushed to `retaskable/login-form`, and Start again begins a fresh session."));
    assert!(p.contains("PR: (none)"));
    a.branch = None;
    let p = triage_prompt(&a, &env(&f, "github", &at_ten_minutes)).unwrap();
    assert!(p.contains("- It has no PR and no work to keep."));
    let bare = triage_prompt(&TriageArgs::default(), &env(&f, "github", &at_ten_minutes)).unwrap();
    assert!(bare.contains("It stopped while `unknown`, because:\n\n(no reason recorded)"));
    assert!(bare.contains("- Specs: (none)") && bare.contains("## Recent history\n\n(none)"));
}

#[test]
fn the_check_prompt_carries_the_first_looks_words() {
    let f = fixture();
    let mut v = args();
    v["look"] = json!("check");
    v["first"] = json!({ "reason": "Only the config is missing", "suggest": "start_over", "instructions": "" });
    let a = TriageArgs::from_job(&job(v));
    let p = triage_prompt(&a, &env(&f, "github", &at_ten_minutes)).unwrap();
    assert!(p.starts_with("You are checking a triager's call for Retaskable."));
    assert!(p.contains("Only the config is missing"));
    assert!(p.contains("start over"), "the suggested button as words");
    assert!(p.contains("(none)"), "no instructions");
    assert!(!p.contains("{{"));
}

#[tokio::test]
async fn it_refuses_with_no_checkout_and_runs_nothing() {
    let f = fixture();
    let mut e = env(&f, "github", &at_ten_minutes);
    e.has_checkout = false;
    let run = triage_job(&job(args()), &e, |_| async { panic!("no session") }).await;
    assert_eq!(
        run.outcome,
        Outcome::refused("This computer has no checkout of the project to look in.")
    );
    assert!(run.bug.is_none());
}

#[tokio::test]
async fn a_first_look_answers_its_triage_and_hands_back_the_retaskable_bug() {
    let f = fixture();
    let looked = json!({
        "decision": "human", "reason": "r", "instructions": "i", "suggest": "skip", "clear": false,
        "retaskableBug": { "title": "Variable not filled", "details": "d", "sameAs": "item-1" }
    });
    let l2 = looked.clone();
    let run = triage_job(
        &job(args()),
        &env(&f, "github", &at_ten_minutes),
        move |spec| async move {
            assert!(spec.prompt.starts_with("You are the triager for Retaskable."));
            assert_eq!(
                spec.schema["required"],
                json!(["decision", "reason", "instructions", "suggest", "clear"])
            );
            Ok(test_result(true, "success", Some(l2), None))
        },
    )
    .await;
    let Outcome::Done { result, .. } = run.outcome else {
        panic!()
    };
    assert_eq!(serde_json::to_value(&result).unwrap(), json!({ "triage": looked }));
    let bug = run.bug.unwrap();
    assert_eq!(
        (bug.id.as_str(), bug.branch.as_deref(), bug.why.as_str()),
        ("login-form", Some("retaskable/login-form"), "CI stayed red")
    );
    assert_eq!(
        (bug.bug.title.as_str(), bug.bug.same_as.as_deref()),
        ("Variable not filled", Some("item-1"))
    );
}

#[tokio::test]
async fn a_look_with_no_bug_has_none_to_send() {
    let f = fixture();
    let run = triage_job(&job(args()), &env(&f, "github", &at_ten_minutes), |_| async {
        Ok(test_result(
            true,
            "success",
            Some(json!({ "decision": "skip", "reason": "r" })),
            None,
        ))
    })
    .await;
    assert!(matches!(run.outcome, Outcome::Done { .. }));
    assert!(run.bug.is_none());
}

#[tokio::test]
async fn a_check_answers_its_check_and_never_a_bug() {
    let f = fixture();
    let mut v = args();
    v["look"] = json!("check");
    v["first"] = json!({ "reason": "r", "suggest": "skip" });
    let run = triage_job(&job(v), &env(&f, "github", &at_ten_minutes), |spec| async move {
        assert_eq!(spec.schema["required"], json!(["agree", "reason", "instructions"]));
        Ok(test_result(
            true,
            "success",
            Some(json!({ "agree": true, "reason": "ok", "instructions": "", "retaskableBug": { "title": "t", "details": "d" } })),
            None,
        ))
    })
    .await;
    let Outcome::Done { result, .. } = run.outcome else {
        panic!()
    };
    assert_eq!(serde_json::to_value(&result).unwrap()["check"]["agree"], true);
    assert!(run.bug.is_none());
}

#[tokio::test]
async fn failures_say_why_a_budget_stop_is_no_retry_and_a_limit_says_when_it_lifts() {
    let f = fixture();
    let e = env(&f, "github", &at_ten_minutes);
    let run = triage_job(&job(args()), &e, |_| async {
        Ok(test_result(false, "error_max_budget_usd", None, None))
    })
    .await;
    assert_eq!(
        run.outcome,
        Outcome::Failed {
            note: "it hit its $1 budget (spent $0.12)".into(),
            no_retry: true,
            limit_hit: None
        }
    );
    let run = triage_job(&job(args()), &e, |_| async {
        let mut r = test_result(false, "error", None, None);
        r.limit_hit = true;
        Ok(r)
    })
    .await;
    assert_eq!(
        run.outcome,
        Outcome::Failed {
            note: "it hit the usage limit".into(),
            no_retry: false,
            limit_hit: Some(LimitHit {
                engine: None,
                until: 600_000
            })
        }
    );
    let run = triage_job(&job(args()), &e, |_| async { Err(anyhow::anyhow!("spawn failed")) }).await;
    assert_eq!(run.outcome, Outcome::failed("it couldn't look into it (spawn failed)"));
    assert!(run.bug.is_none());
}
