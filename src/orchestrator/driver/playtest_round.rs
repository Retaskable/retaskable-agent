//! One computer's turn at grouping the shared after-merge try-out queue
//! (docs/specs/agent/06.4-playtests.md "Grouped into sessions"), run once a minute for every live
//! project ([`super::Machine::playtest_rounds`]): reads `tryouts_list`, and only when
//! [`crate::playtest_groups::should_group`] says it's due, takes the [`crate::playtest_groups::TRYOUT_GROUPS_LOCK`]
//! lock, runs the planner session and sends its grouping back with `tryout_groups`. Still missing
//! (out of this slice): sending `tryout_add` after a merge is what ever puts something in the
//! queue this reads, forwarding a person's playtest answer, and reopening on a Problem — nothing
//! here does any of those.
use std::path::PathBuf;
use std::sync::Arc;

use crate::backlog::{Client, TryoutGroup};
use crate::config::{Engine, EngineChoiceConfig, ProjectConfig};
use crate::engines::claude::sandbox_enabled;
use crate::engines::resolve::resolve_claude_on_path;
use crate::engines::{EngineId, EphemeralSessionConfig, Role, engine_for, engine_of, run_session};
use crate::env::Env;
use crate::link::TokenStore;
use crate::orchestrator::session::{exists, quiet_timeout};
use crate::playtest_groups::{LastGrouped, Planner, PlaytestEntry, TRYOUT_GROUPS_LOCK, plan, should_group, signature};
use crate::telemetry::now_ms;

const TEMPLATE: &str = include_str!("../../../prompts/playtest-groups.md");

/// What one round needs, gathered from the [`super::Machine`] and the [`super::ProjectLoop`] it
/// runs for ([`super::Machine::playtest_rounds`]).
pub(super) struct Job {
    pub id: String,
    pub client: Arc<Client>,
    pub repo: Option<u64>,
    pub project: ProjectConfig,
    pub env: Env,
    pub engine_choice: EngineChoiceConfig,
    pub machine: String,
    pub backlog_url: String,
    pub keychain: Arc<dyn TokenStore>,
}

/// Runs one round for `job`'s project: nothing if there's no repo id yet, no backlog link, the
/// store can't be reached, fewer than [`crate::playtest_groups::MIN_TO_GROUP`] are waiting, or
/// another computer holds the lock. A `Role::Planner` on anything but Claude, or a Claude binary
/// that can't be found, also skips the round rather than failing it loudly: the next minute's pass
/// tries again.
pub(super) async fn run(job: Job) {
    let Some(repo) = job.repo else {
        return;
    };
    let token = match job.keychain.get(&job.backlog_url) {
        Ok(Some(token)) => token,
        Ok(None) => return,
        Err(e) => {
            tracing::warn!(
                "{}: playtest grouping round skipped: reading the backlog link: {e:#}",
                job.id
            );
            return;
        }
    };
    let list = match job.client.tryouts_list(repo, Some(&token)).await {
        Ok(list) => list,
        Err(e) => {
            tracing::warn!("{}: playtest grouping round skipped: tryouts_list failed: {e}", job.id);
            return;
        }
    };
    let waiting: Vec<_> = list.waiting.into_iter().filter(|t| t.on_branch.is_none()).collect();
    let keys: Vec<String> = waiting.iter().map(|t| t.key.clone()).collect();
    let signature = signature(&keys);
    let last = list.groups.as_ref().map(|g| LastGrouped {
        signature: &g.signature,
        at_ms: g.at,
    });
    if !should_group(waiting.len(), &signature, last, now_ms()) {
        return;
    }
    let engine = engine_for(&job.engine_choice, job.project.engines.as_ref(), Role::Planner, None);
    if engine != Engine::Claude {
        tracing::info!(
            "{}: playtest grouping round skipped: only Claude runs a planner session yet",
            job.id
        );
        return;
    }
    let bin = match resolve_claude_on_path(&job.env) {
        Ok(bin) => bin,
        Err(e) => {
            tracing::warn!("{}: playtest grouping round skipped: {e:#}", job.id);
            return;
        }
    };
    if !exists(&bin, &job.env) {
        tracing::warn!(
            "{}: playtest grouping round skipped: the claude binary wasn't found",
            job.id
        );
        return;
    }
    let answer = match job.client.lock(repo, Some(&token), TRYOUT_GROUPS_LOCK, true).await {
        Ok(answer) => answer,
        Err(e) => {
            tracing::warn!("{}: playtest grouping round skipped: taking the lock: {e}", job.id);
            return;
        }
    };
    if answer.by.as_deref() != Some(job.machine.as_str()) {
        return;
    }
    let entries: Vec<PlaytestEntry> = waiting
        .iter()
        .map(|t| PlaytestEntry {
            key: t.key.clone(),
            item_id: t.item_id.clone(),
            title: t.title.clone(),
            what: t.what.clone(),
            steps: t.steps.clone(),
        })
        .collect();
    let cwd = PathBuf::from(&job.project.repo_path);
    let planner = Planner {
        template: TEMPLATE,
        project_type: job.project.project_type,
        session: EphemeralSessionConfig {
            cwd: &cwd,
            budget_usd: job.project.budget.planner_usd,
            model: "sonnet".to_string(),
            local: None,
            engine: Some(EngineId::Claude),
            cursor: None,
            sandbox: sandbox_enabled(job.project.sandbox, cfg!(windows)),
        },
    };
    let quiet = quiet_timeout(job.project.tuning.session_quiet_minutes);
    let planned = plan(&planner, &entries, |spec| async move {
        let engine = engine_of(Some(EngineId::Claude))?;
        run_session(engine.as_ref(), &bin, &spec, "playtest-groups", None, quiet).await
    })
    .await;
    match planned.groups {
        Some(groups) => {
            let wire: Vec<TryoutGroup> = groups
                .into_iter()
                .map(|g| TryoutGroup {
                    title: g.title,
                    why: g.why,
                    keys: g.keys,
                })
                .collect();
            if let Err(e) = job.client.tryout_groups(repo, Some(&token), &signature, &wire).await {
                tracing::warn!("{}: sending the playtest grouping failed: {e}", job.id);
            }
        }
        None => {
            if let Some(note) = planned.note {
                tracing::info!("{}: playtest grouping round didn't group the queue: {note}", job.id);
            }
        }
    }
    if let Err(e) = job.client.lock(repo, Some(&token), TRYOUT_GROUPS_LOCK, false).await {
        tracing::warn!("{}: dropping the playtest grouping lock failed: {e}", job.id);
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Mutex;

    use anyhow::Result;
    use axum::{Json, Router, routing::post};
    use serde_json::{Value, json};

    use super::*;
    use crate::config::{ProjectType, SandboxMode};

    const GROUPED: &str = r#"{"type":"result","subtype":"success","is_error":false,"structured_output":{"groups":[{"title":"Build","why":"same place","keys":["k1","k2","k3","k4"]}]},"total_cost_usd":0.1,"session_id":"s"}"#;

    /// A fake backlog store answering `tryouts_list` with `waiting`, and `chore_lock` the way the
    /// real one does (backlog/tests.rs): `by` is whoever currently holds it, starting as
    /// `lock_owner` (`None` means free), regardless of whether this request's own `take` succeeded.
    /// Every op asked, with its args, is recorded in the returned list in order.
    async fn store(waiting: Value, lock_owner: Option<&str>) -> (String, Arc<Mutex<Vec<(String, Value)>>>) {
        let ops = Arc::new(Mutex::new(Vec::new()));
        let seen = ops.clone();
        let lock_state = Arc::new(Mutex::new(lock_owner.map(str::to_string)));
        let app = Router::new().route(
            "/v1/repos/{id}/ops",
            post(move |Json(body): Json<Value>| {
                let seen = seen.clone();
                let lock_state = lock_state.clone();
                let waiting = waiting.clone();
                async move {
                    let op = body["op"].as_str().unwrap_or("").to_string();
                    let args = body["args"].clone();
                    seen.lock().unwrap().push((op.clone(), args.clone()));
                    let value = match op.as_str() {
                        "tryouts_list" => json!({ "waiting": waiting, "groups": null }),
                        "chore_lock" => {
                            let mut held_by = lock_state.lock().unwrap();
                            if args["take"].as_bool().unwrap_or(false) {
                                if held_by.is_none() {
                                    *held_by = Some("my-pc".to_string());
                                }
                            } else {
                                *held_by = None;
                            }
                            json!({ "held": held_by.is_some(), "by": held_by.clone() })
                        }
                        _ => Value::Null,
                    };
                    Json(json!({ "value": value }))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), ops)
    }

    struct Keys;
    impl TokenStore for Keys {
        fn get(&self, _url: &str) -> Result<Option<String>> {
            Ok(Some("tok".to_string()))
        }
        fn set(&self, _url: &str, _token: &str) -> Result<()> {
            Ok(())
        }
        fn delete(&self, _url: &str) -> Result<()> {
            Ok(())
        }
    }

    fn tryout(key: &str, on_branch: bool) -> Value {
        json!({
            "key": key,
            "itemId": "1",
            "title": "Thing",
            "what": "Look at it",
            "steps": ["Open it"],
            "onBranch": if on_branch { json!({"branch": "b", "sha": "s"}) } else { Value::Null },
        })
    }

    fn fake_name() -> &'static str {
        if cfg!(windows) { "claude.cmd" } else { "claude.sh" }
    }

    fn fake_claude(bin: &Path, out: &Path, lines: &str) {
        std::fs::write(out, lines).unwrap();
        if cfg!(windows) {
            let script = format!("@echo off\r\ntype \"{}\"\r\nexit /b 0\r\n", out.display());
            std::fs::write(bin, script).unwrap();
        } else {
            let script = format!("#!/bin/sh\ncat \"{}\"\nexit 0\n", out.display());
            crate::test_support::write_executable(bin, &script);
        }
    }

    /// A round over a fake store at `base`, spawning the fake `claude` at `bin` with `cwd` as its
    /// project checkout (nothing here reads or writes git, so a plain directory is enough).
    fn job(base: &str, bin: &Path, cwd: &Path) -> Job {
        let mut project = ProjectConfig {
            project_type: ProjectType::Web,
            sandbox: SandboxMode::Off,
            repo_path: cwd.to_string_lossy().into_owned(),
            ..Default::default()
        };
        project.budget.planner_usd = 1.0;
        let bin = bin.to_string_lossy().into_owned();
        let env = Env::new(move |k| (k == "RETASKABLE_CLAUDE").then(|| bin.clone()));
        Job {
            id: "p".into(),
            client: Arc::new(Client::new(base)),
            repo: Some(1),
            project,
            env,
            engine_choice: EngineChoiceConfig::default(),
            machine: "my-pc".into(),
            backlog_url: base.into(),
            keychain: Arc::new(Keys),
        }
    }

    fn ops(seen: &Arc<Mutex<Vec<(String, Value)>>>) -> Vec<String> {
        seen.lock().unwrap().iter().map(|(op, _)| op.clone()).collect()
    }

    #[tokio::test]
    async fn too_few_waiting_skips_the_round_without_touching_the_lock() {
        let waiting = json!([tryout("k1", false), tryout("k2", false), tryout("k3", false)]);
        let (base, seen) = store(waiting, None).await;
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join(fake_name());
        fake_claude(&bin, &dir.path().join("out.jsonl"), GROUPED);
        run(job(&base, &bin, dir.path())).await;
        assert_eq!(ops(&seen), vec!["tryouts_list"]);
    }

    #[tokio::test]
    async fn a_due_round_locks_plans_sends_the_grouping_and_releases_the_lock() {
        let waiting = json!([
            tryout("k1", false),
            tryout("k2", false),
            tryout("k3", false),
            tryout("k4", false),
        ]);
        let (base, seen) = store(waiting, None).await;
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join(fake_name());
        fake_claude(&bin, &dir.path().join("out.jsonl"), GROUPED);
        run(job(&base, &bin, dir.path())).await;
        assert_eq!(
            ops(&seen),
            vec!["tryouts_list", "chore_lock", "tryout_groups", "chore_lock"]
        );
        let recorded = seen.lock().unwrap();
        let groups_args = recorded[2].1.clone();
        assert_eq!(groups_args["signature"], "k1|k2|k3|k4");
        assert_eq!(groups_args["groups"][0]["keys"], json!(["k1", "k2", "k3", "k4"]));
        assert_eq!(recorded[1].1["take"], true);
        assert_eq!(recorded[3].1["take"], false);
    }

    #[tokio::test]
    async fn a_round_already_locked_by_another_machine_is_skipped() {
        let waiting = json!([
            tryout("k1", false),
            tryout("k2", false),
            tryout("k3", false),
            tryout("k4", false),
        ]);
        let (base, seen) = store(waiting, Some("other-pc")).await;
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join(fake_name());
        fake_claude(&bin, &dir.path().join("out.jsonl"), GROUPED);
        run(job(&base, &bin, dir.path())).await;
        assert_eq!(ops(&seen), vec!["tryouts_list", "chore_lock"]);
    }

    #[tokio::test]
    async fn on_branch_entries_are_excluded_from_the_signature_sent() {
        let waiting = json!([
            tryout("k1", false),
            tryout("k2", false),
            tryout("k3", false),
            tryout("k4", false),
            tryout("k5", true),
        ]);
        let (base, seen) = store(waiting, None).await;
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join(fake_name());
        fake_claude(&bin, &dir.path().join("out.jsonl"), GROUPED);
        run(job(&base, &bin, dir.path())).await;
        let recorded = seen.lock().unwrap();
        let groups_args = recorded.iter().find(|(op, _)| op == "tryout_groups").unwrap().1.clone();
        assert_eq!(groups_args["signature"], "k1|k2|k3|k4");
    }
}
