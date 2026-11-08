//! The real [`DraftHost`]: a draft's worktree through `draft_worktree`, the drafter session through
//! `claude -p` (on the drafter's chosen engine), and the PR through `publish`.
//! The session's process tree registers with `process_tree`, so a restart or Quit cuts it.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Result, anyhow};
use futures_util::future::BoxFuture;

use super::driver::{DraftHost, DraftJob};
use super::session::watch_for;
use crate::config::{CursorModels, Engine, EngineChoiceConfig, ModelsConfig, ProjectConfig};
use crate::drafts::land;
use crate::drafts::session::{Session, Start, drafter_tier, parse_result, session_spec};
use crate::drafts::{DrafterResult, titles_text};
use crate::engines::{Role, TranscriptSink, engine_for, engine_of, place, run_session, start_engine};
use crate::env::Env;
use crate::implementer::{Interrupted, publish};
use crate::tiers::{LOCAL, implementer_tier};
use crate::{draft_worktree, paths};

const TEMPLATE: &str = include_str!("../../prompts/drafter.md");

pub struct ClaudeDrafts {
    data_dir: PathBuf,
    choice: EngineChoiceConfig,
    env: Env,
    models: ModelsConfig,
    cursor_models: CursorModels,
    /// The computer's local model, for the tier ladder.
    #[allow(dead_code)] // read by the tier ladder's later pieces
    local: crate::local_model::LocalModel,
}

impl ClaudeDrafts {
    pub fn new(
        data_dir: PathBuf,
        choice: EngineChoiceConfig,
        env: Env,
        models: ModelsConfig,
        cursor_models: CursorModels,
        local: crate::local_model::LocalModel,
    ) -> Arc<Self> {
        Arc::new(Self {
            data_dir,
            choice,
            env,
            models,
            cursor_models,
            local,
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

/// The model named in the first line of the draft's `drafter.jsonl`, if it can be read.
fn started_on_drafter(data_dir: &Path, project: &str, draft_id: &str) -> Option<String> {
    let text = std::fs::read_to_string(paths::run_dir(data_dir, project, draft_id).join("drafter.jsonl")).ok()?;
    let meta: serde_json::Value = serde_json::from_str(text.lines().next()?).ok()?;
    meta.get("model")?.as_str().map(str::to_string)
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
        let (data_dir, choice, env, models, cursor_models, local) = (
            self.data_dir.clone(),
            self.choice.clone(),
            self.env.clone(),
            self.models,
            self.cursor_models.clone(),
            self.local.clone(),
        );
        Box::pin(async move {
            let project = &job.project;
            // The local model places as Claude Code with no local target, like `place` says.
            let chosen = match start_engine(
                job.engine,
                engine_for(&choice, project.engines.as_ref(), Role::Drafter, None),
            ) {
                Engine::Local => Engine::Claude,
                other => other,
            };
            if chosen != Engine::Claude && !job.placement {
                return Err(anyhow!("{chosen} sessions don't run yet"));
            }
            let placed = place(chosen, Role::Drafter, &env, &cursor_models).map_err(anyhow::Error::msg)?;
            let draft = &job.draft;
            let themes = draft.intake.as_ref().map(|i| i.themes.clone()).unwrap_or_default();
            let titles = titles_text(&draft.titles, &themes);
            let up = local.ladder_for(chosen == Engine::Claude);
            // A resumed Claude drafter stays on the backend its transcript's first line names (read before the new
            // session rewrites it): a local one goes back to the model while it's up, else to its Anthropic tier; an
            // Anthropic one, or one with no readable header, never goes local.
            let resumed = job.resume && chosen == Engine::Claude && crate::tiers::tier_kept_on_resume_on();
            let tier = if resumed
                && up
                && started_on_drafter(&data_dir, &project.id_or_derived(), &draft.id).is_some_and(|m| m == LOCAL)
            {
                LOCAL
            } else {
                let ladder = up && !resumed;
                let floored =
                    crate::tiers::limit_floor(job.usage.as_ref(), !resumed && job.sessions.claude_at_limit(), ladder);
                drafter_tier(
                    draft.request.as_deref().unwrap_or(""),
                    implementer_tier(floored.as_ref(), crate::telemetry::now_ms() / 1000, &models, ladder),
                )
            };
            // Held until the session ends.
            let claimed = local.claim(tier).await;
            let spec = session_spec(
                &Session {
                    template: TEMPLATE,
                    request: draft.request.as_deref().unwrap_or(""),
                    titles: &titles,
                    branch: &job.branch,
                    base_branch: &project.base_branch,
                    engine: chosen,
                    tier,
                    budget: &project.budget,
                    cwd: &job.dir,
                    local: claimed.as_ref().map(|(_, target)| target.clone()),
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
            let quiet = watch_for(project, spec.engine.unwrap_or_default());
            let title = draft.titles.first().map(String::as_str);
            let listed = job
                .sessions
                .register_spec(&project.id_or_derived(), &draft.id, title, Role::Drafter, &spec);
            let r = listed
                .scope(run_session(
                    engine.as_ref(),
                    &placed.bin,
                    &spec,
                    &job.session_id,
                    Some(&sink),
                    quiet,
                ))
                .await?;
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
        let host = ClaudeDrafts::new(
            root.join("data"),
            choice,
            env,
            ModelsConfig::default(),
            models,
            crate::local_model::LocalModel::off(),
        );
        let job = |session_id: &str, resume: bool, placement: bool| DraftJob {
            engine: None,
            project: project_in(&root, &repo),
            draft: serde_json::from_value(serde_json::json!({ "id": "d1", "request": "Add charts" })).unwrap(),
            dir: root.clone(),
            branch: "retaskable/spec-d1".into(),
            session_id: session_id.into(),
            resume,
            follow_up: "Revise it.".into(),
            usage: None,
            placement,
            sessions: Default::default(),
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

    const CLAUDE_DONE: &str = r#"{"type":"result","subtype":"success","is_error":false,"structured_output":{"status":"pr_opened","summary":"s"},"session_id":"chat-1"}"#;

    /// Runs a resumed drafter whose earlier `drafter.jsonl` header names `header` (none when `None`) with the usage at
    /// 2% left, on `local`; returns the model the new transcript's header records and the claude stub's arguments.
    async fn resumed_drafter(
        header: Option<&str>,
        local: crate::local_model::LocalModel,
        kept: bool,
    ) -> (String, String) {
        crate::tiers::set_local_ladder_for_test(true);
        crate::tiers::set_tier_kept_on_resume_for_test(kept);
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let repo = crate::test_support::repo_with_origin(&root);
        let env = crate::test_support::stub_engines(&root, CLAUDE_DONE, "", &[]);
        let project = project_in(&root, &repo);
        let run = paths::run_dir(&root.join("data"), &project.id_or_derived(), "d1");
        std::fs::create_dir_all(&run).unwrap();
        if let Some(model) = header {
            let meta = serde_json::json!({ "type": "retaskable_meta", "model": model, "engine": "claude", "at": 1 });
            std::fs::write(run.join("drafter.jsonl"), format!("{meta}\n")).unwrap();
        }
        let host = ClaudeDrafts::new(
            root.join("data"),
            Default::default(),
            env,
            ModelsConfig::default(),
            Default::default(),
            local,
        );
        let usage = serde_json::json!({ "status": "allowed", "utilization": 0.98, "resetsAt": 4_000_000_000_i64 });
        host.run(DraftJob {
            engine: None,
            project,
            draft: serde_json::from_value(serde_json::json!({ "id": "d1", "request": "Add charts" })).unwrap(),
            dir: root.clone(),
            branch: "retaskable/spec-d1".into(),
            session_id: "chat-1".into(),
            resume: true,
            follow_up: "Revise it.".into(),
            usage: Some(usage),
            placement: true,
            sessions: Default::default(),
        })
        .await
        .unwrap();
        let first = std::fs::read_to_string(run.join("drafter.jsonl")).unwrap();
        let meta: serde_json::Value = serde_json::from_str(first.lines().next().unwrap()).unwrap();
        let args = std::fs::read_to_string(root.join("calls.txt")).unwrap();
        (meta["model"].as_str().unwrap().to_string(), args)
    }

    #[tokio::test]
    async fn a_resumed_local_drafter_goes_back_to_the_model_while_it_is_up_and_to_anthropic_when_it_is_down() {
        let (model, _) = resumed_drafter(Some("local"), crate::local_model::LocalModel::up(), true).await;
        assert_eq!(model, "local");
        let down = crate::local_model::LocalModel::up();
        down.set_up(false);
        let (model, args) = resumed_drafter(Some("local"), down, true).await;
        assert_eq!(model, "sonnet");
        assert!(
            args.contains("--model sonnet") && args.contains("--resume chat-1"),
            "{args}"
        );
    }

    #[tokio::test]
    async fn a_resumed_anthropic_drafter_or_one_with_no_header_never_takes_the_local_model() {
        for header in [Some("sonnet"), Some("opus"), None] {
            let (model, _) = resumed_drafter(header, crate::local_model::LocalModel::up(), true).await;
            assert_ne!(model, "local", "{header:?}");
        }
    }

    #[tokio::test]
    async fn a_resume_with_the_flag_off_picks_its_tier_as_before() {
        // Flag Off: a resume with an Anthropic header lands on the local rows, as it did before.
        let (model, _) = resumed_drafter(Some("sonnet"), crate::local_model::LocalModel::up(), false).await;
        assert_eq!(model, "local");
    }

    #[tokio::test]
    async fn a_draft_is_listed_with_no_lane_while_the_drafter_runs_and_not_after() {
        use crate::orchestrator::live::sessions::Sessions;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let repo = crate::test_support::repo_with_origin(&root);
        let go = root.join("go");
        let step = format!("while [ ! -f '{}' ]; do sleep 0.05; done", go.display());
        let env = crate::test_support::stub_engines(&root, DONE, &step, &[]);
        let host = ClaudeDrafts::new(
            root.join("data"),
            Default::default(),
            env,
            ModelsConfig::default(),
            Default::default(),
            crate::local_model::LocalModel::off(),
        );
        let sessions = Sessions::default();
        let job = DraftJob {
            engine: None,
            project: project_in(&root, &repo),
            draft: serde_json::from_value(
                serde_json::json!({ "id": "d1", "request": "Add charts", "titles": ["Charts"] }),
            )
            .unwrap(),
            dir: root.clone(),
            branch: "retaskable/spec-d1".into(),
            session_id: "s1".into(),
            resume: false,
            follow_up: String::new(),
            usage: None,
            placement: true,
            sessions: sessions.clone(),
        };
        let task = tokio::spawn(host.run(job));
        let rows = sessions.rows_when_running().await;
        assert_eq!(rows.len(), 1);
        let info = &rows[0].info;
        assert_eq!(info.item_id, "d1");
        assert_eq!(info.title.as_deref(), Some("Charts"));
        assert_eq!((info.role, info.lane), (crate::engines::Role::Drafter, None));
        std::fs::write(go, "").unwrap();
        let _ = task.await.unwrap();
        assert!(sessions.snapshot().is_empty());
    }
}
