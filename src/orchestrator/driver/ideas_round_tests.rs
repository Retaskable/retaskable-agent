use std::sync::{Arc, Mutex};

use anyhow::Result;

use axum::{Json, Router, routing::post};
use serde_json::{Value, json};

use super::super::playtest_round::test_support::{self, Keys};
use super::*;
use crate::backlog::Client;
use crate::config::{EngineChoiceConfig, ProjectConfig, ProjectType, SandboxMode};
use crate::engines::engine::test_result;
use crate::env::Env;
use crate::runs::RunLocks;

type Seen = Arc<Mutex<Vec<(String, Value)>>>;

/// A fake store: `ideas_list` answers `ideas`; `chore_lock`/`lock_take` as in playtest_round's tests.
async fn store(ideas: Value, lock_owner: Option<&str>, list_fails: bool) -> (String, Seen) {
    let ops: Seen = Arc::new(Mutex::new(Vec::new()));
    let seen = ops.clone();
    let owner = Arc::new(Mutex::new(lock_owner.map(str::to_string)));
    let app = Router::new().route(
        "/v1/repos/{id}/ops",
        post(move |Json(body): Json<Value>| {
            let seen = seen.clone();
            let owner = owner.clone();
            let ideas = ideas.clone();
            async move {
                let op = body["op"].as_str().unwrap_or("").to_string();
                let args = body["args"].clone();
                seen.lock().unwrap().push((op.clone(), args.clone()));
                let value = match op.as_str() {
                    "ideas_list" if list_fails => {
                        return Json(json!({ "error": { "code": "E_X", "message": "down" } }));
                    }
                    "ideas_list" | "ideas_round" => json!({ "round": null, "ideas": ideas }),
                    _ => test_support::lock_answer(&op, &args, &owner).unwrap_or(Value::Null),
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

fn job(base: &str) -> Job {
    let mut project = ProjectConfig {
        project_type: ProjectType::Web,
        sandbox: SandboxMode::Off,
        repo_path: std::env::temp_dir().to_string_lossy().into_owned(),
        ..Default::default()
    };
    project.budget.scout_usd = 0.7;
    Job {
        base: PlaytestJob {
            id: "p".into(),
            client: Arc::new(Client::new(base)),
            repo: Some(1),
            project,
            env: Env::new(|_| None),
            engine_choice: EngineChoiceConfig::default(),
            cursor_models: Default::default(),
            machine: "my-pc".into(),
            backlog_url: base.into(),
            keychain: Arc::new(Keys),
            locks: None,
        },
        planner_dir: None,
        live: vec!["Dark mode".into()],
        audiences: vec![],
        queue: None,
        max_waits: None,
        placement: false,
        placed: None,
    }
}

fn asked() -> Ask {
    Ask {
        fresh: false,
        quiet: false,
        after: Some("Dark mode".into()),
    }
}

fn ops(seen: &Seen) -> Vec<String> {
    seen.lock().unwrap().iter().map(|(op, _)| op.clone()).collect()
}

fn waiting_row(id: &str) -> Value {
    json!({ "id": id, "title": format!("Old {id}"), "state": "waiting", "at": 1,
        "idea": { "experience": "e", "why": "w", "whyNow": "n", "for": [], "waited": 0 } })
}

fn answer_ok() -> Result<SessionResult> {
    let structured = json!({ "status": "ok", "ideas": [{
        "title": "New one", "experience": "e", "why": "w", "whyNow": "n", "for": [] }] });
    let mut r = test_result(true, "success", Some(structured), None);
    r.cost_usd = Some(0.25);
    Ok(r)
}

#[tokio::test]
async fn claim_mode_takes_lists_sends_and_drops_the_chore_lock() {
    let (base, seen) = store(json!([]), None, false).await;
    let spec = Arc::new(Mutex::new(None));
    let s = spec.clone();
    let out = run_with(job(&base), asked(), |sp| async move {
        *s.lock().unwrap() = Some(sp);
        answer_ok()
    })
    .await;
    assert!(
        matches!(out, Outcome::Ok { cost_usd, .. } if cost_usd == 0.25),
        "{out:?}"
    );
    assert_eq!(
        ops(&seen),
        vec!["chore_lock", "ideas_list", "ideas_round", "chore_lock"]
    );
    let recorded = seen.lock().unwrap();
    assert_eq!(recorded[0].1["name"], "ideas");
    assert_eq!(recorded[0].1["take"], true);
    assert_eq!(recorded[3].1["take"], false);
    assert_eq!(recorded[2].1["after"], "Dark mode");
    let spec = spec.lock().unwrap().take().unwrap();
    assert!(spec.readonly);
    assert_eq!(spec.budget_usd, 0.7);
}

#[tokio::test]
async fn runs_mode_takes_and_drops_by_token_without_chore_lock() {
    let (base, seen) = store(json!([]), None, false).await;
    let locks = RunLocks::default();
    let mut j = job(&base);
    j.base.locks = Some(locks.clone());
    let out = run_with(j, asked(), |_| async { answer_ok() }).await;
    assert!(matches!(out, Outcome::Ok { .. }), "{out:?}");
    assert_eq!(ops(&seen), vec!["lock_take", "ideas_list", "ideas_round", "lock_drop"]);
    assert_eq!(seen.lock().unwrap()[3].1["token"], "L1");
    assert!(locks.tokens().is_empty());
}

#[tokio::test]
async fn fresh_sends_only_new_ideas_and_non_fresh_keeps_waiting_ones() {
    for (fresh, count) in [(true, 1), (false, 2)] {
        let (base, seen) = store(json!([waiting_row("a")]), None, false).await;
        let ask = Ask { fresh, ..asked() };
        run_with(job(&base), ask, |_| async { answer_ok() }).await;
        let recorded = seen.lock().unwrap();
        let sent = recorded.iter().find(|(op, _)| op == "ideas_round").unwrap();
        assert_eq!(sent.1["ideas"].as_array().unwrap().len(), count, "fresh={fresh}");
    }
}

#[tokio::test]
async fn a_lock_held_elsewhere_sends_nothing_and_names_the_holder_only_for_an_ask() {
    let (base, seen) = store(json!([]), Some("other-pc"), false).await;
    let out = run_with(job(&base), asked(), |_| async { answer_ok() }).await;
    assert_eq!(
        out,
        Outcome::Skipped("@other-pc's computer is already thinking of ideas: they'll show here when it's done.".into())
    );
    let quiet = Ask { quiet: true, ..asked() };
    let out = run_with(job(&base), quiet, |_| async { answer_ok() }).await;
    assert_eq!(
        out,
        Outcome::Skipped("another computer is already thinking of ideas".into())
    );
    assert!(!ops(&seen).contains(&"ideas_round".to_string()));
    assert!(!ops(&seen).contains(&"ideas_list".to_string()));
}

#[tokio::test]
async fn a_failed_session_sends_nothing_reports_the_error_and_drops_the_lock() {
    let (base, seen) = store(json!([]), None, false).await;
    let out = run_with(job(&base), asked(), |_| async {
        let mut r = test_result(false, "error_max_budget_usd", None, None);
        r.cost_usd = Some(0.7);
        Ok(r)
    })
    .await;
    let Outcome::Failed { error, cost_usd, .. } = out else {
        panic!("{out:?}")
    };
    assert!(error.starts_with("Claude hit its $0.7 budget"), "{error}");
    assert_eq!(cost_usd, 0.7);
    assert_eq!(ops(&seen), vec!["chore_lock", "ideas_list", "chore_lock"]);

    let (base, seen) = store(json!([]), None, false).await;
    let out = run_with(job(&base), asked(), |_| async { Err(anyhow::anyhow!("boom")) }).await;
    let Outcome::Failed { error, .. } = out else {
        panic!("{out:?}")
    };
    assert!(error.contains("couldn't think of ideas"), "{error}");
    assert_eq!(ops(&seen).last().unwrap(), "chore_lock");
}

#[tokio::test]
async fn a_failed_list_ends_the_round_and_still_drops_the_lock() {
    let (base, seen) = store(json!([]), None, true).await;
    let out = run_with(job(&base), asked(), |_| async { answer_ok() }).await;
    let Outcome::Failed { error, .. } = out else {
        panic!("{out:?}")
    };
    assert!(
        error.starts_with("Couldn't read the ideas from the shared backlog ("),
        "{error}"
    );
    assert_eq!(ops(&seen), vec!["chore_lock", "ideas_list", "chore_lock"]);
}

/// A stub for every engine program that logs `<program> <args>` to `calls.txt` and prints nothing useful.
#[cfg(unix)]
fn stubs() -> (tempfile::TempDir, Env) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let mut vars = Vec::new();
    for (var, program) in [
        ("RETASKABLE_CLAUDE", "claude"),
        ("RETASKABLE_CURSOR", "cursor-agent"),
        ("RETASKABLE_PI", "pi"),
        ("RETASKABLE_CODEX", "codex"),
    ] {
        let bin = root.join(format!("{program}.sh"));
        let script = format!(
            "#!/bin/sh\n[ $# -eq 0 ] && exit 0\necho \"{program} $@\" >> '{r}/calls.txt'\ncat >/dev/null\n",
            r = root.display()
        );
        crate::test_support::write_executable(&bin, &script);
        vars.push((var, bin.to_string_lossy().into_owned()));
    }
    (
        dir,
        Env::new(move |k| vars.iter().find(|(v, _)| *v == k).map(|(_, p)| p.clone())),
    )
}

#[cfg(unix)]
#[tokio::test]
async fn the_writer_runs_on_its_chosen_engine_with_placement_on_and_is_skipped_with_it_off() {
    for (engine, program, model) in [
        (crate::config::Engine::Codex, "codex", None),
        (
            crate::config::Engine::Cursor,
            "cursor-agent",
            Some("--model gpt-5-mini"),
        ),
    ] {
        let (base, _) = store(json!([]), None, false).await;
        let (dir, env) = stubs();
        let mut j = job(&base);
        j.base.env = env;
        j.base.engine_choice.roles.writer = Some(engine);
        j.base.cursor_models.scout = Some("gpt-5-mini".into());
        j.placement = true;
        run(j, asked()).await;
        let calls = std::fs::read_to_string(dir.path().join("calls.txt")).unwrap_or_default();
        assert!(calls.starts_with(program), "{program}: {calls}");
        assert!(model.is_none_or(|m| calls.contains(m)), "{calls}");
        assert_eq!(calls.lines().count(), 1, "{calls}");

        let (dir, env) = stubs();
        let mut j = job(&base);
        j.base.env = env;
        j.base.engine_choice.roles.writer = Some(engine);
        let out = run(j, asked()).await;
        assert!(matches!(out, Outcome::Skipped(_)), "{out:?}");
        assert!(!dir.path().join("calls.txt").exists());
    }
}
