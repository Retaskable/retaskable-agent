use super::*;
use crate::orchestrator::session::tests::{fake_gh, fake_name, project_in};

struct Rig {
    _dir: tempfile::TempDir,
    root: PathBuf,
    host: Arc<ClaudeTrial>,
    project: ProjectConfig,
}

/// A real repo with an origin, a `claude` that saves its prompt and arguments then prints `answer`, and a fake `gh`
/// (failing, when `gh_fails`). `claude_missing` points `RETASKABLE_CLAUDE` at nothing.
fn rig(answer: &str, gh_fails: bool, claude_missing: bool) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let repo = crate::test_support::repo_with_origin(&root);
    let r = root.display();
    let bin = root.join(fake_name());
    crate::test_support::write_executable(
        &bin,
        &format!(
            "#!/bin/sh\n[ $# -eq 0 ] && exit 0\ncat > '{r}/prompt.txt'\necho \"$@\" >> '{r}/args'\ncat '{r}/out.jsonl'\n"
        ),
    );
    std::fs::write(root.join("out.jsonl"), answer).unwrap();
    let gh = root.join("gh.sh");
    if gh_fails {
        crate::test_support::write_executable(&gh, "#!/bin/sh\nexit 1\n");
    } else {
        fake_gh(&gh, &root);
    }
    let mut project = project_in(&root, &repo);
    project.budget.drafter_usd = 2.0;
    let claude = if claude_missing {
        root.join("nope").to_string_lossy().into_owned()
    } else {
        bin.to_string_lossy().into_owned()
    };
    let gh = gh.to_string_lossy().into_owned();
    let env = Env::new(move |k| match k {
        "RETASKABLE_CLAUDE" => Some(claude.clone()),
        "RETASKABLE_GH" => Some(gh.clone()),
        _ => None,
    });
    let host = ClaudeTrial::new(
        root.join("data"),
        EngineChoiceConfig::default(),
        env,
        ModelsConfig::default(),
    );
    Rig {
        _dir: dir,
        root,
        host,
        project,
    }
}

fn answer(status: &str) -> String {
    format!(
        r#"{{"type":"result","subtype":"success","is_error":false,"structured_output":{{"status":"{status}","summary":"s"}},"session_id":"s"}}"#
    )
}

async fn run(rig: &Rig) -> TrialEnd {
    let job = TrialJob {
        project: rig.project.clone(),
        lane: 1,
        branch: "retaskable/trial-plan".into(),
        session_id: crate::implementer::new_session_id(),
        usage: None,
    };
    rig.host.run(job).await.unwrap()
}

fn origin_has_branch(rig: &Rig) -> bool {
    let out = std::process::Command::new("git")
        .arg("--git-dir")
        .arg(rig.root.join("origin.git"))
        .args(["branch", "--list", "retaskable/trial-plan"])
        .output()
        .unwrap();
    !out.stdout.is_empty()
}

fn pr_created(rig: &Rig) -> bool {
    std::fs::read_to_string(rig.root.join("gh.log")).is_ok_and(|l| l.contains("pr create"))
}

#[tokio::test]
async fn pr_opened_pushes_and_returns_the_pr() {
    let rig = rig(&answer("pr_opened"), false, false);
    let end = run(&rig).await;
    assert_eq!(
        end,
        TrialEnd::Opened {
            pr_url: "https://github.com/o/p/pull/7".into()
        }
    );
    assert!(origin_has_branch(&rig));
    let prompt = std::fs::read_to_string(rig.root.join("prompt.txt")).unwrap();
    assert!(prompt.contains(TRIAL_TEXT), "{prompt}");
    assert!(!prompt.contains("## Where to start"), "no spec files: {prompt}");
    let args = std::fs::read_to_string(rig.root.join("args")).unwrap();
    assert!(!args.contains("--dangerously-skip-permissions"), "{args}");
    assert!(args.contains("--permission-mode auto"), "{args}");
    assert!(args.contains("--max-budget-usd 2"), "{args}");
    let gh = std::fs::read_to_string(rig.root.join("gh.log")).unwrap();
    assert!(gh.contains(TRIAL_TITLE), "{gh}");
}

#[tokio::test]
async fn other_answers_are_dropped_and_open_no_pr() {
    for status in ["needs_clarification", "failed"] {
        let rig = rig(&answer(status), false, false);
        let TrialEnd::Dropped { reason } = run(&rig).await else {
            panic!("{status} should drop")
        };
        assert!(reason.contains(status), "{reason}");
        assert!(!pr_created(&rig));
        assert!(!origin_has_branch(&rig));
    }
}

#[tokio::test]
async fn a_session_that_wont_start_is_dropped() {
    let rig = rig(&answer("pr_opened"), false, true);
    let TrialEnd::Dropped { reason } = run(&rig).await else {
        panic!("should drop")
    };
    assert!(reason.contains("claude"), "{reason}");
    assert!(!pr_created(&rig));
}

#[tokio::test]
async fn a_failing_publish_is_dropped() {
    let rig = rig(&answer("pr_opened"), true, false);
    let TrialEnd::Dropped { reason } = run(&rig).await else {
        panic!("should drop")
    };
    assert!(reason.contains("couldn't be published"), "{reason}");
}
