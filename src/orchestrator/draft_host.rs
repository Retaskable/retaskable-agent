//! The real [`DraftHost`]: a draft's worktree through `draft_worktree`, the drafter session through
//! `claude -p` (on the drafter's chosen engine), and the PR through `publish`.
//! The session's process tree registers with `process_tree`, so a restart or Quit cuts it.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use futures_util::future::BoxFuture;

use super::driver::{DraftHost, DraftJob};
use super::session::watch_for;
use crate::ask::classify::{Classified, Options, classify};
use crate::config::{CursorModels, Engine, EngineChoiceConfig, ProjectConfig};
use crate::design_board::DesignError;
use crate::design_board::session::{layout_spec, sentence_spec};
use crate::drafts::failure::{failure_reason, retry_text};
use crate::drafts::land;
use crate::drafts::session::{Session, Start, drafter_tier, parse_result, session_spec};
use crate::drafts::{DrafterResult, titles_text};
use crate::engines::cursor::with_project;
use crate::engines::engine::{LocalTarget, SessionSpec};
use crate::engines::{Role, TranscriptSink, describe_failure, engine_for, engine_of, place, run_session, start_engine};
use crate::env::Env;
use crate::implementer::{Interrupted, publish};
use crate::tiers::{LOCAL, SONNET, floor};
use crate::{draft_worktree, paths};

const TEMPLATE: &str = include_str!("../../prompts/drafter.md");

pub struct ClaudeDrafts {
    data_dir: PathBuf,
    choice: EngineChoiceConfig,
    env: Env,
    cursor_models: CursorModels,
    /// The computer's local model, for the tier ladder.
    local: crate::local_model::LocalModel,
}

impl ClaudeDrafts {
    pub fn new(
        data_dir: PathBuf,
        choice: EngineChoiceConfig,
        env: Env,
        cursor_models: CursorModels,
        local: crate::local_model::LocalModel,
    ) -> Arc<Self> {
        Arc::new(Self {
            data_dir,
            choice,
            env,
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
        let (data_dir, choice, env, cursor_models, local) = (
            self.data_dir.clone(),
            self.choice.clone(),
            self.env.clone(),
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
            let placed = place(chosen, Role::Drafter, &env, &cursor_models).map_err(anyhow::Error::msg)?;
            let draft = &job.draft;
            let themes = draft.intake.as_ref().map(|i| i.themes.clone()).unwrap_or_default();
            // A plan that revises an item is told about it after the pieces (card-revisions.md section 3.3).
            let titles = [
                titles_text(&draft.titles, &themes),
                draft.revising.clone().unwrap_or_default(),
            ]
            .into_iter()
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
            let up = local.ladder_for(chosen == Engine::Claude);
            // A resumed Claude drafter stays on the backend its transcript's first line names (read before the new
            // session rewrites it): a local one goes back to the model while it's up, else to its Anthropic tier; an
            // Anthropic one, or one with no readable header, never goes local.
            let resumed = job.resume && chosen == Engine::Claude;
            let tier = if resumed
                && up
                && started_on_drafter(&data_dir, &project.id_or_derived(), &draft.id).is_some_and(|m| m == LOCAL)
            {
                LOCAL
            } else {
                let ladder = up && !resumed;
                drafter_tier(
                    draft.request.as_deref().unwrap_or(""),
                    floor(SONNET, !resumed && job.sessions.claude_at_limit(), ladder),
                )
            };
            let retried = !job.resume && draft.other.get("retried").and_then(|v| v.as_bool()) == Some(true);
            let retry = if retried {
                retry_text(failure_reason(draft).as_deref())
            } else {
                String::new()
            };
            // Held until the session ends.
            let claimed = local.claim(tier).await;
            let request =
                crate::issues::finish::drafter_request(draft.issue.as_ref(), draft.request.as_deref().unwrap_or(""));
            let spec = session_spec(
                &Session {
                    template: TEMPLATE,
                    request: &request,
                    titles: &titles,
                    branch: &job.branch,
                    base_branch: &project.base_branch,
                    retry: &retry,
                    engine: chosen,
                    tier,
                    budget: &project.budget,
                    cwd: &job.dir,
                    local: claimed.as_ref().map(|(_, target)| target.clone()),
                    cursor: placed.cursor.clone().map(|c| with_project(project, c)),
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
                append: false,
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

    fn merge(&self, project: &ProjectConfig, pr: u64, delete_branch: bool, squash: bool) -> Result<()> {
        let method = if squash {
            "--squash"
        } else {
            land::workflow(project).method_flag
        };
        let (gh, dir) = (self.gh(), Path::new(&project.repo_path));
        // `squash` is only ever a person's Merge into main (`driver/land.rs`): it goes in as the head stood a moment
        // ago, so a push since then is refused. The TS agent's `mergeForPerson` passes none (`merge.ts:318`).
        let head = if squash {
            Some(land::pr_head(&gh, dir, pr)?)
        } else {
            None
        };
        land::merge_pr(&gh, dir, pr, method, delete_branch, head.as_deref())
    }

    fn close(&self, project: &ProjectConfig, pr: u64) -> Result<()> {
        land::close_pr(&self.gh(), Path::new(&project.repo_path), pr)
    }

    fn pr_state(&self, project: &ProjectConfig, pr: u64) -> Result<land::PrState> {
        land::pr_state(&self.gh(), Path::new(&project.repo_path), pr)
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

    fn design_files(&self, project: &ProjectConfig, dir: &Path) -> Result<Vec<crate::drafts::files::DraftFile>> {
        crate::drafts::files::draft_files(dir, &project.base_branch)
    }

    /// One read-only Claude session on the scout tier with no shell, in a scratch directory (`design_board::session`).
    fn design_layout(
        &self,
        project: &ProjectConfig,
        prompt: String,
    ) -> BoxFuture<'static, Result<serde_json::Value, DesignError>> {
        let (env, models, budget) = (self.env.clone(), self.cursor_models.clone(), project.budget.ask_usd);
        Box::pin(design_session(env, models, budget, prompt, layout_spec, "a layout"))
    }

    /// The same kind of session, asked for one plain sentence again (`design_board::keep`).
    fn design_sentence(
        &self,
        project: &ProjectConfig,
        prompt: String,
    ) -> BoxFuture<'static, Result<serde_json::Value, DesignError>> {
        let (env, models, budget) = (self.env.clone(), self.cursor_models.clone(), project.budget.ask_usd);
        Box::pin(design_session(env, models, budget, prompt, sentence_spec, "a sentence"))
    }

    fn classify(&self, project: &ProjectConfig, text: String) -> BoxFuture<'static, Classified> {
        let (env, models, project) = (self.env.clone(), self.cursor_models.clone(), project.clone());
        // What the project's sessions cost today on this computer, so a spent day's budget skips the call.
        let spent =
            crate::store::MachineStore::load(&self.data_dir).map_or(0.0, |m| m.spend_today(&project.id_or_derived()));
        Box::pin(async move {
            let Ok(placed) = place(Engine::Claude, Role::Assistant, &env, &models) else {
                return Classified::Unavailable;
            };
            let opts = Options::new(true, false, project.budget.ask_usd, spent);
            let cwd = std::env::temp_dir();
            classify(&text, &opts, &cwd, |spec| async move {
                let engine = engine_of(spec.engine)?;
                run_session(
                    engine.as_ref(),
                    &placed.bin,
                    &spec,
                    "classify",
                    None,
                    Default::default(),
                )
                .await
            })
            .await
        })
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
            models,
            crate::local_model::LocalModel::off(),
        );
        let job = |session_id: &str, resume: bool| DraftJob {
            engine: None,
            project: project_in(&root, &repo),
            draft: serde_json::from_value(serde_json::json!({ "id": "d1", "request": "Add charts" })).unwrap(),
            dir: root.clone(),
            branch: "retaskable/spec-d1".into(),
            session_id: session_id.into(),
            resume,
            follow_up: "Revise it.".into(),
            usage: None,
            sessions: Default::default(),
        };
        let first = host.run(job("fresh-id", false)).await.unwrap();
        assert_eq!(first.session_id.as_deref(), Some("chat-1"));
        host.run(job("chat-1", true)).await.unwrap();
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
    async fn resumed_drafter(header: Option<&str>, local: crate::local_model::LocalModel) -> (String, String) {
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
        let host = ClaudeDrafts::new(root.join("data"), Default::default(), env, Default::default(), local);
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
        let (model, _) = resumed_drafter(Some("local"), crate::local_model::LocalModel::up()).await;
        assert_eq!(model, "local");
        let down = crate::local_model::LocalModel::up();
        down.set_up(false);
        let (model, args) = resumed_drafter(Some("local"), down).await;
        assert_eq!(model, "sonnet");
        assert!(
            args.contains("--model sonnet") && args.contains("--resume chat-1"),
            "{args}"
        );
    }

    #[tokio::test]
    async fn a_resumed_anthropic_drafter_or_one_with_no_header_never_takes_the_local_model() {
        for header in [Some("sonnet"), Some("opus"), None] {
            let (model, _) = resumed_drafter(header, crate::local_model::LocalModel::up()).await;
            assert_ne!(model, "local", "{header:?}");
        }
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

/// One read-only session of the design board (`designSession`): the answer's structured part, or the reason there is none.
async fn design_session(
    env: Env,
    models: CursorModels,
    budget: f64,
    prompt: String,
    spec: fn(String, &str, f64, std::path::PathBuf, Option<LocalTarget>) -> SessionSpec,
    answer: &str,
) -> Result<serde_json::Value, DesignError> {
    let placed = place(Engine::Claude, Role::Assistant, &env, &models)
        .map_err(|e| DesignError::conflict(format!("the designs can't be drawn: {e:#}")))?;
    let spec = spec(prompt, "haiku", budget, std::env::temp_dir(), None);
    let engine = engine_of(spec.engine).map_err(|e| DesignError::conflict(format!("{e:#}")))?;
    let result = run_session(engine.as_ref(), &placed.bin, &spec, "design", None, Default::default())
        .await
        .map_err(|e| DesignError::conflict(format!("{e:#}")))?;
    if !result.ok {
        return Err(DesignError::conflict(crate::text::truncate(
            &format!("Claude {}.", describe_failure(&result, budget)),
            280,
        )));
    }
    result
        .structured
        .ok_or_else(|| DesignError::conflict(format!("Claude finished without {answer}.")))
}
