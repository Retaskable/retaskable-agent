//! The real [`DraftHost`]: a draft's worktree through `draft_worktree`, the drafter session through
//! `claude -p` (on the drafter's chosen engine), and the PR through `publish`.
//! The session's process tree registers with `process_tree`, so a restart or Quit cuts it.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Result, anyhow};
use futures_util::future::BoxFuture;

use super::driver::{DraftHost, DraftJob};
use super::session::quiet_timeout;
use crate::config::{CursorModels, Engine, EngineChoiceConfig, ModelsConfig, ProjectConfig};
use crate::drafts::land;
use crate::drafts::session::{Session, Start, is_lighthouse, parse_result, session_spec};
use crate::drafts::{DrafterResult, titles_text};
use crate::engines::{Role, TranscriptSink, engine_for, engine_of, place, run_session};
use crate::env::Env;
use crate::implementer::{Interrupted, publish};
use crate::tiers::implementer_tier;
use crate::{draft_worktree, paths};

const TEMPLATE: &str = include_str!("../../prompts/drafter.md");

pub struct ClaudeDrafts {
    data_dir: PathBuf,
    choice: EngineChoiceConfig,
    env: Env,
    models: ModelsConfig,
    cursor_models: CursorModels,
}

impl ClaudeDrafts {
    pub fn new(
        data_dir: PathBuf,
        choice: EngineChoiceConfig,
        env: Env,
        models: ModelsConfig,
        cursor_models: CursorModels,
    ) -> Arc<Self> {
        Arc::new(Self {
            data_dir,
            choice,
            env,
            models,
            cursor_models,
        })
    }
}

impl ClaudeDrafts {
    fn gh(&self) -> PathBuf {
        self.env
            .get("RETASKABLE_GH")
            .map_or_else(|| PathBuf::from("gh"), PathBuf::from)
    }
}

impl DraftHost for ClaudeDrafts {
    fn prepare(&self, project: &ProjectConfig, id: &str, branch: &str) -> Result<PathBuf> {
        draft_worktree::create(
            Path::new(&project.repo_path),
            Path::new(&project.worktrees_dir),
            id,
            branch,
            &project.base_branch,
        )
    }

    fn run(&self, job: DraftJob) -> BoxFuture<'static, Result<DrafterResult>> {
        let (data_dir, choice, env, models, cursor_models) = (
            self.data_dir.clone(),
            self.choice.clone(),
            self.env.clone(),
            self.models,
            self.cursor_models.clone(),
        );
        Box::pin(async move {
            let project = &job.project;
            let chosen = engine_for(&choice, project.engines.as_ref(), Role::Drafter, None);
            if chosen != Engine::Claude && !job.placement {
                return Err(anyhow!("{chosen} sessions don't run yet"));
            }
            let placed = place(chosen, Role::Drafter, &env, &cursor_models).map_err(anyhow::Error::msg)?;
            let draft = &job.draft;
            let themes = draft.intake.as_ref().map(|i| i.themes.clone()).unwrap_or_default();
            let titles = titles_text(&draft.titles, &themes);
            let tier = implementer_tier(job.usage.as_ref(), crate::telemetry::now_ms() / 1000, &models);
            let spec = session_spec(
                &Session {
                    template: TEMPLATE,
                    request: draft.request.as_deref().unwrap_or(""),
                    titles: &titles,
                    branch: &job.branch,
                    base_branch: &project.base_branch,
                    engine: chosen,
                    tier,
                    chore: job.lighthouse && draft.request.as_deref().is_some_and(is_lighthouse),
                    budget: &project.budget,
                    cwd: &job.dir,
                    local: None,
                    cursor: placed.cursor.clone(),
                },
                if job.resume {
                    Start::Resume {
                        session_id: job.session_id.clone(),
                        follow_up: &job.follow_up,
                        decisions: &[],
                    }
                } else {
                    Start::Fresh
                },
            )?;
            let sink = TranscriptSink {
                path: paths::run_dir(&data_dir, &project.id_or_derived(), &draft.id).join("drafter.jsonl"),
                lane: None,
            };
            let engine = engine_of(spec.engine)?;
            let quiet = quiet_timeout(project.tuning.session_quiet_minutes);
            let r = run_session(engine.as_ref(), &placed.bin, &spec, &job.session_id, Some(&sink), quiet).await?;
            if r.subtype == "interrupted" {
                return Err(anyhow::Error::new(Interrupted(
                    "the drafter was cut by a restart".into(),
                )));
            }
            parse_result(r, project.budget.drafter_usd)
        })
    }

    fn publish(
        &self,
        project: &ProjectConfig,
        dir: &Path,
        branch: &str,
        title: &str,
        body: &str,
    ) -> Result<publish::Published> {
        publish::publish(
            crate::git_bin::program(),
            &self.gh(),
            dir,
            branch,
            &project.base_branch,
            title,
            body,
        )
    }

    fn remove(&self, project: &ProjectConfig, id: &str) {
        if let Err(e) = draft_worktree::remove(Path::new(&project.repo_path), Path::new(&project.worktrees_dir), id) {
            tracing::warn!("{id}: removing the draft's worktree: {e:#}");
        }
    }

    fn merge(&self, project: &ProjectConfig, pr: u64, delete_branch: bool) -> Result<()> {
        let method = land::workflow(project).method_flag;
        land::merge_pr(&self.gh(), Path::new(&project.repo_path), pr, method, delete_branch)
    }

    fn close(&self, project: &ProjectConfig, pr: u64) -> Result<()> {
        land::close_pr(&self.gh(), Path::new(&project.repo_path), pr)
    }

    fn rebase(&self, project: &ProjectConfig, id: &str, branch: &str) -> Result<bool> {
        let dir = self.prepare(project, id, branch)?;
        land::rebase(crate::git_bin::program(), &dir, branch, &project.base_branch)
    }

    fn push(&self, project: &ProjectConfig, id: &str, branch: &str) -> Result<()> {
        let dir = self.prepare(project, id, branch)?;
        land::push_rebased(crate::git_bin::program(), &dir, branch)
    }

    fn fetch(&self, project: &ProjectConfig) -> Result<()> {
        crate::lanes::fetch_latest(Path::new(&project.repo_path)).map(|_| ())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::orchestrator::session::tests::project_in;

    const DONE: &str = r#"{"type":"result","subtype":"success","is_error":false,"result":"```json\n{\"status\":\"pr_opened\",\"summary\":\"s\"}\n```","session_id":"chat-1"}"#;

    #[tokio::test]
    async fn a_cursor_drafter_runs_on_the_top_model_and_a_revision_resumes_its_chat() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let repo = crate::test_support::repo_with_origin(&root);
        let r = root.display();
        let bin = root.join("cursor.sh");
        crate::test_support::write_executable(
            &bin,
            &format!(
                "#!/bin/sh\n[ $# -eq 0 ] && exit 0\necho \"$@\" >> '{r}/args'\ncat >/dev/null\ncat '{r}/out.jsonl'\n"
            ),
        );
        std::fs::write(root.join("out.jsonl"), DONE).unwrap();
        let bin = bin.to_string_lossy().into_owned();
        let env = Env::new(move |k| (k == "RETASKABLE_CURSOR").then(|| bin.clone()));
        let mut choice = EngineChoiceConfig::default();
        choice.roles.drafter = Some(Engine::Cursor);
        let models = CursorModels {
            top: Some("gpt-5".into()),
            ..Default::default()
        };
        let host = ClaudeDrafts::new(root.join("data"), choice, env, ModelsConfig::default(), models);
        let job = |session_id: &str, resume: bool, placement: bool| DraftJob {
            project: project_in(&root, &repo),
            draft: serde_json::from_value(serde_json::json!({ "id": "d1", "request": "Add charts" })).unwrap(),
            dir: root.clone(),
            branch: "retaskable/spec-d1".into(),
            session_id: session_id.into(),
            resume,
            follow_up: "Revise it.".into(),
            usage: None,
            placement,
            lighthouse: false,
        };
        assert!(host.run(job("fresh-id", false, false)).await.is_err());
        let first = host.run(job("fresh-id", false, true)).await.unwrap();
        assert_eq!(first.session_id.as_deref(), Some("chat-1"));
        host.run(job("chat-1", true, true)).await.unwrap();
        let args = std::fs::read_to_string(root.join("args")).unwrap();
        let (fresh, revision) = args.trim().split_once('\n').unwrap();
        assert!(
            fresh.contains("--model gpt-5") && !fresh.contains("--resume"),
            "{fresh}"
        );
        assert!(
            revision.contains("--model gpt-5 ") && revision.contains("--resume chat-1"),
            "{revision}"
        );
    }
}
