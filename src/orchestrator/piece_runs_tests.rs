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
    let mut script = format!(
        "#!/bin/sh\n[ $# -eq 0 ] && exit 0\nR='{}'\nn=$(cat \"$R/count\" 2>/dev/null || echo 0); n=$((n+1)); echo $n > \"$R/count\"\necho \"$@\" >> \"$R/args_all\"\ncat > \"$R/prompt$n.txt\"\ncase $n in\n",
        root.display()
    );
    for (i, step) in steps.iter().enumerate() {
        script += &format!("{}) {step};;\n", i + 1);
    }
    script += &format!("esac\ncat \"{}\"\n", root.join("out.jsonl").display());
    let bin = root.join(fake_name());
    crate::test_support::write_executable(&bin, &script);
    std::fs::write(root.join("out.jsonl"), answer).unwrap();
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
        kind,
        piece: serde_json::from_value(piece).unwrap(),
        lane: Some(lanes::lane_path(&rig.root.join("worktrees"), 1)),
        ends,
        context: crate::orchestrator::driver::RunContext {
            project: "p".into(),
            config: Default::default(),
            client: Arc::new(crate::backlog::Client::new("http://127.0.0.1:1")),
            repo: 1,
            token: "tok".into(),
            locks: Default::default(),
        },
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

#[tokio::test]
async fn a_fix_run_starts_from_the_pushed_branch() {
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
    let (req, mut s) = request(&r, RunKind::Fix, p);
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
async fn a_build_run_for_a_piece_with_a_pushed_branch_carries_on_from_it() {
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
    let (req, mut s) = request(&r, RunKind::Build, p);
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

#[test]
fn it_asks_for_build_and_fix() {
    let r = rig(&[], "");
    assert_eq!(r.starter.kinds(), vec![RunKind::Build, RunKind::Fix]);
}
