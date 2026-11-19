//! The fix session's prompt and spec: the TS agent's words, and the implementer lane's settings.
use super::*;
use crate::config::SandboxMode;
use crate::engines::SessionStart;
use crate::implementer::implementer_result_schema;

fn project() -> ProjectConfig {
    let mut p = ProjectConfig::default();
    p.budget.implementer_usd = 7.5;
    p.budget.scout_usd = 0.4;
    p.sandbox = SandboxMode::Off;
    p
}

fn spec_for(p: &ProjectConfig, cwd: &Path) -> SessionSpec {
    let s = FixSession {
        project: p,
        cwd,
        model: "sonnet".into(),
        max_turns: 300,
        engine: Some(EngineId::Claude),
        cursor: None,
        local: None,
    };
    session_spec(
        12,
        "Fix the thing",
        "feat/x",
        &FixTask::Red("check `build` failed".into()),
        &[],
        &s,
    )
    .unwrap()
}

#[test]
fn the_prompt_is_the_typescript_agents_words_then_the_commit_only_rule() {
    let spec = spec_for(&project(), Path::new("w"));
    assert_eq!(
        spec.prompt
            .strip_suffix(crate::implementer::session::COMMIT_ONLY)
            .expect("ends with the rule"),
        "Pull request #12 \"Fix the thing\" (branch feat/x) is failing CI because of its own change. Fix it in this worktree, which is on the branch.\n\n\
         The last CI run failed because:\n\n\
         check `build` failed\n\n\
         Commit the fix. Do not push, do not open or change a pull request, do not force anything. Run the checks that failed before you finish. Return status \"pr_opened\" with a summary once it is committed: say briefly what you changed and whether the checks passed."
    );
}

#[test]
fn the_session_is_an_implementer_write_session_in_the_worktree() {
    let cwd = Path::new("worktrees").join("merge-fix-12");
    let spec = spec_for(&project(), &cwd);
    assert_eq!(spec.cwd, cwd);
    assert_eq!(
        (spec.model.as_str(), spec.budget_usd, spec.max_turns),
        ("sonnet", 7.5, Some(300))
    );
    assert_eq!(spec.schema, implementer_result_schema());
    assert!(!spec.readonly && spec.shell && !spec.sandbox);
    assert_eq!(spec.engine, Some(EngineId::Claude));
    // Its own fresh session: never a resume of a lane's.
    assert!(matches!(spec.start, SessionStart::Fresh(_)));
}

#[test]
fn the_sandbox_follows_the_projects_setting() {
    let mut p = project();
    p.sandbox = SandboxMode::Auto;
    let spec = spec_for(&p, Path::new("w"));
    assert_eq!(spec.sandbox, sandbox_enabled(SandboxMode::Auto, cfg!(windows)));
}

#[test]
fn the_worktree_is_merge_fix_and_the_pull_request() {
    assert_eq!(
        fix_dir(Path::new("trees"), 9, "0123456789abcdef"),
        Path::new("trees").join("merge-fix-9-01234567")
    );
}

#[test]
fn two_heads_of_one_pull_request_have_their_own_worktrees() {
    let (a, b) = (
        fix_dir(Path::new("trees"), 9, "aaaaaaaa11"),
        fix_dir(Path::new("trees"), 9, "bbbbbbbb22"),
    );
    assert_ne!(a, b);
    assert_ne!(a, fix_dir(Path::new("trees"), 10, "aaaaaaaa11"));
}

fn prior() -> Vec<CiPrior> {
    vec![
        CiPrior {
            kind: "fix".into(),
            pushed: true,
            summary: "Changed the parser.".into(),
            ci: Some(crate::backlog::ci_result::CiPriorRun {
                sha: "abcdef0123".into(),
                excerpt: "not ok 3 - parse".into(),
            }),
            ..Default::default()
        },
        CiPrior {
            kind: "mergeMain".into(),
            summary: "Resolved a conflict.".into(),
            ..Default::default()
        },
    ]
}

#[test]
fn earlier_attempts_and_their_ci_failures_are_in_the_prompt_with_a_different_approach_rule() {
    let p = project();
    let s = FixSession {
        project: &p,
        cwd: Path::new("w"),
        model: "opus".into(),
        max_turns: 300,
        engine: Some(EngineId::Claude),
        cursor: None,
        local: None,
    };
    let spec = session_spec(12, "T", "b", &FixTask::Red("what failed".into()), &prior(), &s).unwrap();
    let t = &spec.prompt;
    assert!(t.contains("The last CI run failed because:\n\nwhat failed"), "{t}");
    assert!(t.contains("## Earlier attempts and why they didn't work"), "{t}");
    assert!(
        t.contains("Attempt 1 (a fix, pushed) did: Changed the parser.\nand then failed because (CI run abcdef01):"),
        "{t}"
    );
    assert!(t.contains("not ok 3 - parse"), "{t}");
    assert!(
        t.contains("Attempt 2 (merging main, not pushed) did: Resolved a conflict."),
        "{t}"
    );
    assert!(t.contains("Take a different approach from those"), "{t}");
    // The merge brief carries them too; none means no section.
    let merge = session_spec(12, "T", "b", &FixTask::Merge("conflicts".into()), &prior(), &s).unwrap();
    assert!(merge.prompt.contains("Earlier attempts and why they didn't work"));
    assert!(!spec_for(&project(), Path::new("w")).prompt.contains("Earlier attempts"));
}

#[test]
fn the_summary_is_the_sessions_own_report_else_its_error() {
    use crate::engines::engine::SessionResult;
    let said = SessionResult {
        structured: Some(serde_json::json!({ "status": "pr_opened", "summary": " Fixed the import; checks passed. " })),
        ..Default::default()
    };
    assert_eq!(summary_of(&said).as_deref(), Some("Fixed the import; checks passed."));
    let broke = SessionResult {
        error: Some("ran out of turns".into()),
        ..Default::default()
    };
    assert_eq!(summary_of(&broke).as_deref(), Some("ran out of turns"));
    assert_eq!(summary_of(&SessionResult::default()), None);
}
