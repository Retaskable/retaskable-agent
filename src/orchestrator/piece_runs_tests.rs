use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc::UnboundedReceiver;

use super::*;
use crate::orchestrator::driver::RunEndSender;
use crate::orchestrator::session::tests::{fake_gh, fake_name, project_in};

const COMMIT: &str = "echo 'fn a() {}' > small.rs; git add -A; git -c user.name=t -c user.email=t@t commit -qm small";

struct Rig {
    _dir: tempfile::TempDir,
    root: PathBuf,
    starter: Arc<PieceStarter>,
}

/// A project over a real repo whose `claude` saves each prompt as `prompt<n>.txt`, does `steps[n-1]` in the lane
/// (shell), then prints `answer`.
fn rig(steps: &[&str], answer: &str) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let repo = crate::test_support::repo_with_origin(&root);
    // A reviewer session (its prompt says so) is counted apart and answers from `review<m>.jsonl` (else the
    // approving `review_default.jsonl`); `review<m>.sleep` makes it hang.
    let mut script = format!(
        "#!/bin/sh\n[ $# -eq 0 ] && exit 0\nR='{}'\ncat > \"$R/in.txt\"\nif grep -q 'You are the final reviewer' \"$R/in.txt\"; then\nm=$(cat \"$R/count_r\" 2>/dev/null || echo 0); m=$((m+1)); echo $m > \"$R/count_r\"\ncp \"$R/in.txt\" \"$R/reviewer$m.txt\"\n[ -f \"$R/review$m.sleep\" ] && sleep 60\nf=\"$R/review$m.jsonl\"; [ -f \"$f\" ] || f=\"$R/review_default.jsonl\"\ncat \"$f\"; exit 0\nfi\nn=$(cat \"$R/count\" 2>/dev/null || echo 0); n=$((n+1)); echo $n > \"$R/count\"\necho \"$@\" >> \"$R/args_all\"\ncp \"$R/in.txt\" \"$R/prompt$n.txt\"\ncase $n in\n",
        root.display()
    );
    for (i, step) in steps.iter().enumerate() {
        script += &format!("{}) {step};;\n", i + 1);
    }
    script += &format!("esac\ncat \"{}\"\n", root.join("out.jsonl").display());
    let bin = root.join(fake_name());
    crate::test_support::write_executable(&bin, &script);
    std::fs::write(root.join("out.jsonl"), answer).unwrap();
    std::fs::write(root.join("review_default.jsonl"), reviewed("approve", "Fine.")).unwrap();
    let gh = root.join("gh.sh");
    fake_gh(&gh, &root);
    let project = project_in(&root, &repo);
    let (bin, gh) = (bin.to_string_lossy().into_owned(), gh.to_string_lossy().into_owned());
    let env = Env::new(move |k| match k {
        "RETASKABLE_CLAUDE" => Some(bin.clone()),
        "RETASKABLE_GH" => Some(gh.clone()),
        _ => None,
    });
    let starter = Arc::new(PieceStarter {
        data_dir: root.join("data"),
        projects: vec![project],
        choice: EngineChoiceConfig::default(),
        env,
        max_turns: 50,
        models: ModelsConfig::default(),
        flags: Arc::new(crate::flags::Flags::fixed(&[])),
    });
    Rig {
        _dir: dir,
        root,
        starter,
    }
}

fn answer(status: &str, extra: &str) -> String {
    format!(
        r#"{{"type":"result","subtype":"success","is_error":false,"structured_output":{{"status":"{status}","summary":"s"{extra}}},"session_id":"s"}}"#
    )
}

struct Started {
    ends: UnboundedReceiver<RunEnd>,
    trees: UnboundedReceiver<(String, Arc<crate::process_tree::ProcessTree>)>,
}

fn request(rig: &Rig, kind: RunKind, piece: Value) -> (RunRequest, Started) {
    let (ends, rx) = RunEndSender::channel();
    let (processes, trees) = crate::orchestrator::driver::RunProcesses::channel();
    let request = RunRequest {
        token: "tok".into(),
        project: "p".into(),
        processes,
        stop: Default::default(),
        kind,
        piece: serde_json::from_value(piece).unwrap(),
        lane: Some(lanes::lane_path(&rig.root.join("worktrees"), 1)),
        ends,
        context: crate::orchestrator::driver::RunContext::for_test(),
    };
    (request, Started { ends: rx, trees })
}

fn piece() -> Value {
    json!({ "id": "pc1", "title": "Show the thing", "what": "Show it.", "doneWhen": "It shows." })
}

async fn end_of(rx: &mut UnboundedReceiver<RunEnd>) -> RunEnd {
    tokio::time::timeout(Duration::from_secs(120), rx.recv())
        .await
        .expect("an end")
        .expect("the channel stays open")
}

fn origin_has(rig: &Rig, branch: &str) -> bool {
    let out = std::process::Command::new("git")
        .arg("--git-dir")
        .arg(rig.root.join("origin.git"))
        .args(["branch", "--list", branch])
        .output()
        .unwrap();
    !out.stdout.is_empty()
}

const BRANCH: &str = "retaskable/pc1-show-the-thing";

#[tokio::test]
async fn a_build_run_opens_the_change_and_ends_pr_opened() {
    let r = rig(&[COMMIT], &answer("pr_opened", ""));
    let (req, mut s) = request(&r, RunKind::Build, piece());
    r.starter.start(req).unwrap();
    let end = end_of(&mut s.ends).await;
    assert_eq!(end.outcome, RunOutcome::PrOpened);
    assert_eq!(end.result["branch"], BRANCH);
    assert_eq!(end.result["change"]["number"], 7);
    assert!(origin_has(&r, BRANCH));
    let prompt = std::fs::read_to_string(r.root.join("prompt1.txt")).unwrap();
    assert!(
        prompt.contains("Show the thing") && prompt.contains("It shows."),
        "{prompt}"
    );
    let args = std::fs::read_to_string(r.root.join("args_all")).unwrap();
    assert!(
        args.contains("--permission-mode auto") && !args.contains("--dangerously"),
        "{args}"
    );
    let files = std::fs::read_dir(paths::run_dir(&r.root.join("data"), "p", "pc1")).unwrap();
    assert_eq!(files.count(), 1);
}

/// A run of `kind` for a piece whose branch is already pushed: it carries on from that branch.
async fn carries_on_from_the_pushed_branch(kind: RunKind) {
    let r = rig(
        &[
            "test -f pushed.rs && echo 'fn b() {}' > more.rs; git add -A; git -c user.name=t -c user.email=t@t commit -qm more",
        ],
        &answer("pr_opened", ""),
    );
    let repo = r.root.join("repo");
    let git = |args: &[&str]| {
        assert!(
            std::process::Command::new("git")
                .current_dir(&repo)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    };
    git(&["checkout", "-q", "-b", "pushed-branch"]);
    std::fs::write(repo.join("pushed.rs"), "fn p() {}").unwrap();
    git(&["add", "-A"]);
    git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "pushed"]);
    git(&["push", "-q", "origin", "pushed-branch"]);
    git(&["checkout", "-q", "main"]);
    let mut p = piece();
    p["branch"] = json!("pushed-branch");
    p["next"] = json!({ "what": "Fix the red check." });
    let (req, mut s) = request(&r, kind, p);
    r.starter.start(req).unwrap();
    let end = end_of(&mut s.ends).await;
    assert_eq!(end.outcome, RunOutcome::PrOpened, "{:?}", end.result);
    assert_eq!(end.result["branch"], "pushed-branch");
    let tree = std::process::Command::new("git")
        .arg("--git-dir")
        .arg(r.root.join("origin.git"))
        .args(["ls-tree", "--name-only", "pushed-branch"])
        .output()
        .unwrap();
    let tree = String::from_utf8_lossy(&tree.stdout);
    assert!(tree.contains("pushed.rs") && tree.contains("more.rs"), "{tree}");
    let prompt = std::fs::read_to_string(r.root.join("prompt1.txt")).unwrap();
    assert!(prompt.contains("Fix the red check."), "{prompt}");
}

#[tokio::test]
async fn a_fix_run_starts_from_the_pushed_branch() {
    carries_on_from_the_pushed_branch(RunKind::Fix).await;
}

#[tokio::test]
async fn a_build_run_for_a_piece_with_a_pushed_branch_carries_on_from_it() {
    carries_on_from_the_pushed_branch(RunKind::Build).await;
}

#[tokio::test]
async fn questions_after_a_commit_push_first() {
    let q = r#","questions":[{"question":"Which?","options":["a","b"]}]"#;
    let r = rig(&[COMMIT], &answer("needs_clarification", q));
    let (req, mut s) = request(&r, RunKind::Build, piece());
    r.starter.start(req).unwrap();
    let end = end_of(&mut s.ends).await;
    assert_eq!(end.outcome, RunOutcome::Questions);
    assert_eq!(end.result["branch"], BRANCH);
    assert!(origin_has(&r, BRANCH));
    assert_eq!(end.result["questions"][0]["question"], "Which?");
}

#[tokio::test]
async fn questions_before_any_commit_push_nothing() {
    let q = r#","questions":[{"question":"Which?","options":["a","b"]}]"#;
    let r = rig(&["true"], &answer("needs_clarification", q));
    let (req, mut s) = request(&r, RunKind::Build, piece());
    r.starter.start(req).unwrap();
    let end = end_of(&mut s.ends).await;
    assert_eq!(end.outcome, RunOutcome::Questions);
    assert!(end.result.get("branch").is_none());
    assert!(!origin_has(&r, BRANCH));
}

#[tokio::test]
async fn a_starter_error_gives_the_run_back() {
    let r = rig(&[], &answer("pr_opened", ""));
    let (mut req, _s) = request(&r, RunKind::Build, piece());
    req.project = "nope".into();
    assert!(r.starter.start(req).unwrap_err().contains("no project nope"));
    let mut p = piece();
    p["engine"] = json!("codex");
    let (req, _s) = request(&r, RunKind::Build, p);
    assert!(r.starter.start(req).unwrap_err().contains("only Claude"));
}

#[tokio::test]
async fn cutting_the_registered_tree_ends_the_run_interrupted() {
    let r = rig(&["sleep 60"], &answer("pr_opened", ""));
    let (req, mut s) = request(&r, RunKind::Build, piece());
    r.starter.start(req).unwrap();
    let (token, tree) = tokio::time::timeout(Duration::from_secs(60), s.trees.recv())
        .await
        .expect("the session's tree")
        .unwrap();
    assert_eq!(token, "tok");
    tree.cut();
    tree.kill_and_wait(crate::process_tree::KILL_LIMIT);
    let end = end_of(&mut s.ends).await;
    assert_eq!(end.outcome, RunOutcome::Interrupted);
}

fn check(name: &str, command: &str, blocking: bool) -> crate::config::CheckConfig {
    crate::config::CheckConfig {
        name: name.into(),
        command: command.into(),
        blocking,
        timeout_sec: 60,
    }
}

/// `rig` with the project's checks and rounds set.
fn rig_checked(steps: &[&str], checks: Vec<crate::config::CheckConfig>, rounds: u32) -> Rig {
    let mut r = rig(steps, &answer("pr_opened", ""));
    let starter = Arc::get_mut(&mut r.starter).unwrap();
    starter.projects[0].checks = checks;
    starter.projects[0].max_check_rounds = rounds;
    r
}

async fn end_with(r: &Rig) -> RunEnd {
    let (req, mut s) = request(r, RunKind::Build, piece());
    r.starter.start(req).unwrap();
    end_of(&mut s.ends).await
}

const FIXED: &str = "touch fixed.txt; git add -A; git -c user.name=t -c user.email=t@t commit -qm fixed";

#[tokio::test]
async fn passing_checks_let_the_run_end_pr_opened_with_their_evidence() {
    let r = rig_checked(&[COMMIT], vec![check("ok", "true", true)], 3);
    let end = end_with(&r).await;
    assert_eq!(end.outcome, RunOutcome::PrOpened);
    assert_eq!(end.result["checks_summary"], "✓ ok");
    assert!(end.result["diff_stat"].as_str().unwrap().contains("small.rs"));
}

#[tokio::test]
async fn a_blocking_failure_goes_back_to_the_session_and_a_second_round_passes() {
    let c = check("needs-fix", "echo still-broken; test -f fixed.txt", true);
    let r = rig_checked(&[COMMIT, FIXED], vec![c], 3);
    let end = end_with(&r).await;
    assert_eq!(end.outcome, RunOutcome::PrOpened);
    let prompt = std::fs::read_to_string(r.root.join("prompt2.txt")).unwrap();
    assert!(
        prompt.contains("still-broken") && prompt.contains("test -f fixed.txt"),
        "{prompt}"
    );
    assert_eq!(end.result["checks_summary"], "✓ needs-fix");
}

#[tokio::test]
async fn a_non_blocking_failure_does_not_block() {
    let r = rig_checked(&[COMMIT], vec![check("lint", "false", false)], 3);
    let end = end_with(&r).await;
    assert_eq!(end.outcome, RunOutcome::PrOpened);
    assert_eq!(end.result["checks_summary"], "✗ lint (non-blocking)");
    assert!(!r.root.join("prompt2.txt").exists());
}

#[tokio::test]
async fn failing_past_max_check_rounds_fails_naming_the_checks() {
    let checks = vec![
        check("unit", "false", true),
        check("types", "false", true),
        check("lint", "false", false),
    ];
    let r = rig_checked(&[COMMIT, "true"], checks, 1);
    let end = end_with(&r).await;
    let RunOutcome::Failed(why) = end.outcome else {
        panic!("{:?}", end.outcome)
    };
    assert!(why.contains("unit, types") && !why.contains("lint"), "{why}");
    assert!(!origin_has(&r, BRANCH));
}

#[tokio::test]
async fn a_failed_install_counts_as_a_failed_blocking_check() {
    let step = r#"echo '{ not json' > package.json; git add -A; git -c user.name=t -c user.email=t@t commit -qm pkg"#;
    let r = rig_checked(&[step, "true"], vec![check("ok", "true", true)], 1);
    let end = end_with(&r).await;
    let RunOutcome::Failed(why) = end.outcome else {
        panic!("{:?}", end.outcome)
    };
    assert!(why.contains("npm"), "{why}");
    assert!(
        std::fs::read_to_string(r.root.join("prompt2.txt"))
            .unwrap()
            .contains("npm")
    );
}

#[test]
fn it_asks_for_build_and_fix() {
    let r = rig(&[], "");
    assert_eq!(r.starter.kinds(), vec![RunKind::Build, RunKind::Fix]);
}

fn reviewed(verdict: &str, summary: &str) -> String {
    format!(
        r#"{{"type":"result","subtype":"success","is_error":false,"structured_output":{{"verdict":"{verdict}","summary":"{summary}","comments":[{{"body":"rename the thing"}}]}},"session_id":"r"}}"#
    )
}

const COMMIT_B: &str = "echo 'fn b() {}' > b.rs; git add -A; git -c user.name=t -c user.email=t@t commit -qm b";

fn script_review(r: &Rig, m: u32, verdict: &str) {
    std::fs::write(
        r.root.join(format!("review{m}.jsonl")),
        reviewed(verdict, "Needs work."),
    )
    .unwrap();
}

fn comment(r: &Rig) -> String {
    std::fs::read_to_string(r.root.join("gh_comment")).unwrap_or_default()
}

#[tokio::test]
async fn an_approving_review_posts_its_comment_and_ends_pr_opened() {
    let r = rig(&[COMMIT], &answer("pr_opened", ""));
    let (req, mut s) = request(&r, RunKind::Build, piece());
    r.starter.start(req).unwrap();
    let end = end_of(&mut s.ends).await;
    assert_eq!(end.outcome, RunOutcome::PrOpened);
    assert!(comment(&r).contains("**Verdict:** approve"), "{}", comment(&r));
    assert!(
        std::fs::read_to_string(r.root.join("gh.log"))
            .unwrap()
            .contains("pr comment 7")
    );
    let prompt = std::fs::read_to_string(r.root.join("reviewer1.txt")).unwrap();
    assert!(prompt.contains("The piece: Show it."), "{prompt}");
}

#[tokio::test]
async fn a_request_for_changes_resumes_the_implementer_then_re_reviews() {
    let r = rig(&[COMMIT, COMMIT_B], &answer("pr_opened", ""));
    script_review(&r, 1, "request_changes");
    let (req, mut s) = request(&r, RunKind::Build, piece());
    r.starter.start(req).unwrap();
    let end = end_of(&mut s.ends).await;
    assert_eq!(end.outcome, RunOutcome::PrOpened);
    let fix = std::fs::read_to_string(r.root.join("prompt2.txt")).unwrap();
    assert!(fix.contains("rename the thing"), "{fix}");
    assert!(
        std::fs::read_to_string(r.root.join("args_all"))
            .unwrap()
            .lines()
            .nth(1)
            .unwrap()
            .contains("--resume")
    );
    let again = std::fs::read_to_string(r.root.join("reviewer2.txt")).unwrap();
    assert!(again.contains("re-review"), "{again}");
    assert_eq!(comment(&r).matches("**Verdict:**").count(), 2);
    let log = std::fs::read_to_string(r.root.join("gh.log")).unwrap();
    assert_eq!(log.matches("pr comment").count(), 2);
}

#[tokio::test]
async fn too_many_review_rounds_end_failed_with_the_requests() {
    let mut r = rig(&[COMMIT, COMMIT_B], &answer("pr_opened", ""));
    Arc::get_mut(&mut r.starter).unwrap().projects[0].max_review_rounds = 1;
    script_review(&r, 1, "request_changes");
    script_review(&r, 2, "request_changes");
    let (req, mut s) = request(&r, RunKind::Build, piece());
    r.starter.start(req).unwrap();
    let end = end_of(&mut s.ends).await;
    let RunOutcome::Failed(why) = end.outcome else {
        panic!("{:?}", end.outcome)
    };
    assert!(
        why.contains("after 1 rounds") && why.contains("rename the thing"),
        "{why}"
    );
}

#[tokio::test]
async fn a_verdict_needing_a_person_ends_failed_with_why() {
    let r = rig(&[COMMIT], &answer("pr_opened", ""));
    script_review(&r, 1, "needs_human");
    let (req, mut s) = request(&r, RunKind::Build, piece());
    r.starter.start(req).unwrap();
    let RunOutcome::Failed(why) = end_of(&mut s.ends).await.outcome else {
        panic!("not failed")
    };
    assert!(why.contains("needs a person") && why.contains("Needs work."), "{why}");
}

#[tokio::test]
async fn a_verdict_needing_a_person_with_questions_ends_questions() {
    let r = rig(&[COMMIT], &answer("pr_opened", ""));
    let line = r#"{"type":"result","subtype":"success","is_error":false,"structured_output":{"verdict":"needs_human","summary":"s","questions":[{"question":"Which way?","options":["a","b"]}]},"session_id":"s"}"#;
    std::fs::write(r.root.join("review1.jsonl"), line).unwrap();
    let (req, mut s) = request(&r, RunKind::Build, piece());
    r.starter.start(req).unwrap();
    let end = end_of(&mut s.ends).await;
    assert_eq!(end.outcome, RunOutcome::Questions);
    assert_eq!(end.result["questions"][0]["question"], "Which way?");
    assert_eq!(end.result["change"]["number"], 7);
}

#[tokio::test]
async fn cutting_a_review_session_ends_the_run_interrupted() {
    let r = rig(&[COMMIT], &answer("pr_opened", ""));
    std::fs::write(r.root.join("review1.sleep"), "").unwrap();
    let (req, mut s) = request(&r, RunKind::Build, piece());
    r.starter.start(req).unwrap();
    let mut last = None;
    for _ in 0..2 {
        last = Some(
            tokio::time::timeout(Duration::from_secs(60), s.trees.recv())
                .await
                .expect("a session's tree")
                .unwrap(),
        );
    }
    let (_, tree) = last.unwrap();
    tree.cut();
    tree.kill_and_wait(crate::process_tree::KILL_LIMIT);
    assert_eq!(end_of(&mut s.ends).await.outcome, RunOutcome::Interrupted);
    assert!(comment(&r).is_empty());
}
