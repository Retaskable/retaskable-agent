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
    session_spec(12, "Fix the thing", "feat/x", "check `build` failed", &s).unwrap()
}

#[test]
fn the_prompt_is_the_typescript_agents_words() {
    let spec = spec_for(&project(), Path::new("w"));
    assert_eq!(
        spec.prompt,
        "Pull request #12 \"Fix the thing\" (branch feat/x) is failing CI because of its own change. Fix it in this worktree, which is on the branch.\n\n\
         check `build` failed\n\n\
         Commit the fix. Do not push, do not open or change a pull request, do not force anything. Return status \"pr_opened\" with a summary once it is committed."
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
