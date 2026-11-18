//! `.agent-ci/` or a GitLab CI file on the base branch (`ciCoverage`'s `ciFile`, `agentCiIn`; ci-when-ready.md section 10).
use super::ci;
use super::test_rig::{Rig, rig};
use crate::ci_coverage::{agent_ci_in, covering, has_ci_file, workflows_tree};
use crate::item_lifecycle::{ItemRun, ItemState, ci_when_ready_active};
use crate::orchestrator::steps::{StepDone, StepRequest};

const HEAD: &str = "aaaa111";

/// Commits `path` to `origin/main` in the rig's repo.
fn on_main(r: &Rig, path: &str) {
    let repo = std::path::PathBuf::from(&r.ctx.project.repo_path);
    let file = repo.join(path);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "x\n").unwrap();
    for args in [
        ["add", "-A"].as_slice(),
        &["commit", "-qm", "ci"],
        &["push", "-q", "origin", "main"],
    ] {
        let out = crate::spawn::command("git")
            .current_dir(&repo)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    }
}

/// A project whose only CI is the agent-ci runner: no workflows, `requireCi` at its add-project default, `.agent-ci/` read from the base.
fn agent_ci_rig() -> Rig {
    let mut r = rig(vec![]);
    on_main(&r, ".agent-ci/ci.sh");
    r.ci_gh(HEAD);
    let tree = workflows_tree(std::path::Path::new(&r.ctx.project.repo_path), "origin/main");
    r.ctx.project.agent_ci = agent_ci_in(tree.as_deref());
    r
}

fn request(r: &Rig) -> StepRequest {
    let mut request = r.request(ItemState::AwaitingCi);
    request.flow.pr_number = Some(7);
    request
}

async fn step(r: &Rig, request: StepRequest) -> StepDone {
    ci::step(r.ctx.clone(), request).await
}

#[tokio::test]
async fn an_agent_ci_project_with_no_workflows_labels_the_approved_change_and_waits_for_ci() {
    let r = agent_ci_rig();
    assert!(!r.ctx.project.require_ci && r.ctx.project.ci_when_ready.is_none());
    assert!(ci_when_ready_active(&r.ctx.project));
    // Finishing the work goes to the checks (CI starts beside them); an approval goes to `awaiting_ci`, not `merging`.
    let mut run = ItemRun::default();
    run.implementer_finished(crate::item_lifecycle::ImplementerOutcome::Finished, &r.ctx.project);
    assert_eq!(run.state, ItemState::Checking);
    // Approved with no label on: it goes on, and the step keeps waiting.
    let mut req = request(&r);
    req.flow.approved_sha = Some(HEAD.into());
    let done = step(&r, req).await;
    assert_eq!(done.run.state, ItemState::AwaitingCi);
    assert!(done.flow.unwrap().ci_ready_at.is_some());
    assert!(
        r.gh_calls()
            .iter()
            .any(|c| c.contains("labels") && c.contains("ci-ready"))
    );
}

#[tokio::test]
async fn an_agent_ci_project_does_not_merge_before_ci_passes_on_the_approved_head() {
    let r = agent_ci_rig();
    let mut req = request(&r);
    req.flow.approved_sha = Some(HEAD.into());
    req.flow.ci_ready_at = Some(1);
    req.flow.ci_wait_since = Some(super::host::now_ms());
    r.runs(&[("build", "in_progress", None)], "2026-11-01T10:05:00Z");
    assert_eq!(step(&r, req.clone()).await.run.state, ItemState::AwaitingCi);
    r.runs(&[("build", "completed", Some("success"))], "2026-11-01T10:05:00Z");
    assert_eq!(step(&r, req).await.run.state, ItemState::Merging);
}

#[tokio::test]
async fn the_settings_beat_the_agent_ci_default() {
    let mut r = agent_ci_rig();
    r.ctx.project.ci_when_ready = Some(false);
    assert!(!ci_when_ready_active(&r.ctx.project));
    r.ctx.project.ci_when_ready = None;
    assert!(ci_when_ready_active(&r.ctx.project));
}

#[tokio::test]
async fn a_project_with_neither_keeps_its_behaviour() {
    let mut r = rig(vec![]);
    r.ci_gh(HEAD);
    assert!(!r.ctx.project.agent_ci && !ci_when_ready_active(&r.ctx.project));
    assert_eq!(step(&r, request(&r)).await.run.state, ItemState::Checking);
    r.ctx.project.require_ci = true;
    let done = step(&r, request(&r)).await;
    assert!(done.notes[0].starts_with("No CI runs for pull requests into main"));
    let mut run = ItemRun::default();
    run.implementer_finished(crate::item_lifecycle::ImplementerOutcome::Finished, &r.ctx.project);
    assert_eq!(run.state, ItemState::AwaitingCi);
}

#[test]
fn a_gitlab_ci_file_or_agent_ci_covers_and_is_read_from_the_base() {
    use crate::ci_coverage::CiAnswer;
    assert_eq!(covering(&[], "main", true).answer, CiAnswer::Covered);
    assert_eq!(covering(&[], "main", false).answer, CiAnswer::None);
    let r = rig(vec![]);
    let repo = std::path::PathBuf::from(&r.ctx.project.repo_path);
    assert!(!has_ci_file(&repo, "origin/main"));
    on_main(&r, ".gitlab-ci.yml");
    assert!(has_ci_file(&repo, "origin/main"));
    let tree = workflows_tree(&repo, "origin/main");
    assert!(
        !agent_ci_in(tree.as_deref()),
        "a GitLab CI file is not the agent-ci runner"
    );
    assert!(agent_ci_in(Some("- - - abc123")));
    assert!(!agent_ci_in(Some("- abc123 - -")) && !agent_ci_in(None));
}
