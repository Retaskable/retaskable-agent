//! The minute pass starting each project's daily vet pass in the background.
use super::*;
use std::path::Path;

type Seen = Arc<Mutex<Vec<(String, Value)>>>;

struct Knobs {
    due: bool,
    /// Who the store says holds the lock.
    holder: &'static str,
    suggestions: Vec<&'static str>,
    /// A verdict for this suggestion id is refused as already answered.
    answered: &'static str,
    /// A verdict for this suggestion id fails.
    broken: &'static str,
}

impl Default for Knobs {
    fn default() -> Self {
        Knobs {
            due: true,
            holder: "my-pc",
            suggestions: vec!["a", "b", "c"],
            answered: "",
            broken: "",
        }
    }
}

async fn store(k: Knobs) -> (String, Seen) {
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let k = Arc::new(k);
    let app = Router::new().route(
        "/v1/repos/{id}/ops",
        post(move |Json(body): Json<Value>| {
            let (log, k) = (log.clone(), k.clone());
            async move {
                use axum::response::IntoResponse;
                let op = body["op"].as_str().unwrap_or("").to_string();
                let args = body["args"].clone();
                log.lock().unwrap().push((op.clone(), args.clone()));
                let value = match op.as_str() {
                    "jobs_due" => json!({ "day": "2026-11-03", "due": if k.due { json!(["vet"]) } else { json!([]) } }),
                    "chore_lock" => json!({ "held": args["take"], "by": k.holder }),
                    "suggestions_ask" => json!(
                        k.suggestions
                            .iter()
                            .map(|id| json!({ "id": id, "text": "t", "at": "2026-11-02" }))
                            .collect::<Vec<_>>()
                    ),
                    "list" => json!([]),
                    "ideas_list" => json!({ "round": null, "ideas": [] }),
                    "suggestion_verdict" => {
                        let id = args["id"].as_str().unwrap_or("");
                        if id == k.answered {
                            return (
                                axum::http::StatusCode::CONFLICT,
                                Json(json!({ "error": "already_answered" })),
                            )
                                .into_response();
                        }
                        if id == k.broken {
                            return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(json!({}))).into_response();
                        }
                        json!({})
                    }
                    _ => Value::Null,
                };
                Json(json!({ "value": value })).into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), seen)
}

fn fake_claude(dir: &Path, out: &str) -> String {
    let bin = dir.join(if cfg!(windows) { "claude.cmd" } else { "claude.sh" });
    let file = dir.join("out.jsonl");
    std::fs::write(&file, out).unwrap();
    if cfg!(windows) {
        std::fs::write(
            &bin,
            format!("@echo off\r\ntype \"{}\"\r\nexit /b 0\r\n", file.display()),
        )
        .unwrap();
    } else {
        crate::test_support::write_executable(&bin, &format!("#!/bin/sh\ncat \"{}\"\nexit 0\n", file.display()));
    }
    bin.to_string_lossy().into_owned()
}

fn verdicts_json(ids: &[&str]) -> String {
    let verdicts: Vec<Value> = ids
        .iter()
        .enumerate()
        .map(|(i, id)| json!({ "id": id, "verdict": if i == 0 { "promising" } else { "spam" }, "reason": format!("r-{id}") }))
        .collect();
    json!({"type":"result","subtype":"success","is_error":false,
        "structured_output": { "verdicts": verdicts }, "total_cost_usd": 0.1, "session_id": "s"})
    .to_string()
}

fn live_rig(base: &str, flag: bool, enabled: bool, out: &str) -> Rig {
    let mut rig = rig(2, &[base]);
    rig.go_live();
    let bin = fake_claude(rig._dir.path(), out);
    rig.machine.links.env = Env::new(move |k| (k == "RETASKABLE_CLAUDE").then(|| bin.clone()));
    let cwd = rig._dir.path().to_string_lossy().into_owned();
    rig.lp(0).project.repo_path = cwd;
    rig.lp(0).project.sandbox = crate::config::SandboxMode::Off;
    rig.lp(0).project.vet_suggestions.enabled = enabled;
    rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(if flag {
        &["chore-vetting-suggestions"]
    } else {
        &[]
    }));
    rig
}

async fn minute(rig: &mut Rig) {
    let steps = rig.machine.pass().await;
    rig.machine.vet_rounds(&steps).await;
}

async fn settle() {
    tokio::time::sleep(Duration::from_millis(500)).await;
}

fn ops(seen: &Seen) -> Vec<String> {
    let mut ops: Vec<String> = seen.lock().unwrap().iter().map(|(o, _)| o.clone()).collect();
    ops.retain(|o| {
        [
            "jobs_due",
            "chore_lock",
            "suggestions_ask",
            "ideas_list",
            "suggestion_verdict",
            "run_take",
        ]
        .contains(&o.as_str())
    });
    ops
}

fn sent_ids(seen: &Seen) -> Vec<String> {
    let seen = seen.lock().unwrap();
    (seen.iter())
        .filter(|(o, _)| o == "suggestion_verdict")
        .map(|(_, a)| a["id"].as_str().unwrap().to_string())
        .collect()
}

fn locks(seen: &Seen) -> Vec<(String, bool)> {
    let seen = seen.lock().unwrap();
    (seen.iter())
        .filter(|(o, _)| o == "chore_lock")
        .map(|(_, a)| (a["name"].as_str().unwrap().to_string(), a["take"].as_bool().unwrap()))
        .collect()
}

#[tokio::test]
async fn flag_off_setting_off_or_no_token_does_nothing() {
    for (flag, enabled, token) in [(false, true, true), (true, false, true), (true, true, false)] {
        let (base, seen) = store(Knobs::default()).await;
        let mut rig = live_rig(&base, flag, enabled, &verdicts_json(&["a"]));
        if !token {
            *rig.keys.token.lock().unwrap() = None;
        }
        minute(&mut rig).await;
        settle().await;
        assert_eq!(ops(&seen), Vec::<String>::new(), "{flag} {enabled} {token}");
    }
}

#[tokio::test]
async fn not_due_asks_nothing_else() {
    let (base, seen) = store(Knobs {
        due: false,
        ..Knobs::default()
    })
    .await;
    let mut rig = live_rig(&base, true, true, &verdicts_json(&["a"]));
    minute(&mut rig).await;
    settle().await;
    assert_eq!(ops(&seen), vec!["jobs_due"]);
}

#[tokio::test]
async fn a_due_day_takes_its_lock_sends_verdicts_in_order_and_drops_the_lock() {
    let (base, seen) = store(Knobs::default()).await;
    let mut rig = live_rig(&base, true, true, &verdicts_json(&["b", "a", "c"]));
    minute(&mut rig).await;
    settle().await;
    assert_eq!(sent_ids(&seen), vec!["b", "a", "c"]);
    assert_eq!(
        locks(&seen),
        vec![
            ("vet-2026-11-02".to_string(), false),
            ("vet-2026-11-03".to_string(), true),
            ("vet-2026-11-03".to_string(), false)
        ]
    );
    let ops = ops(&seen);
    assert_eq!(ops.last().map(String::as_str), Some("chore_lock"));
    assert!(!ops.contains(&"run_take".to_string()));
    let state = rig.lp(0).vet_daily.clone();
    let st = state.lock().unwrap();
    let last = st.last.clone().unwrap();
    assert_eq!(last.day, "2026-11-03");
    assert_eq!(last.counts.get("promising"), Some(&1));
    assert_eq!(last.counts.get("spam"), Some(&2));
    assert_eq!(st.recent.len(), 3);
    assert_eq!(st.recent[0].reason, "r-b");
}

#[tokio::test]
async fn another_holder_skips_without_asking_for_suggestions() {
    let (base, seen) = store(Knobs {
        holder: "other-pc",
        ..Knobs::default()
    })
    .await;
    let mut rig = live_rig(&base, true, true, &verdicts_json(&["a"]));
    minute(&mut rig).await;
    settle().await;
    let ops = ops(&seen);
    assert!(!ops.contains(&"suggestions_ask".to_string()), "{ops:?}");
    assert!(sent_ids(&seen).is_empty());
}

#[tokio::test]
async fn no_suggestions_starts_no_session() {
    let (base, seen) = store(Knobs {
        suggestions: vec![],
        ..Knobs::default()
    })
    .await;
    let mut rig = live_rig(&base, true, true, &verdicts_json(&["a"]));
    minute(&mut rig).await;
    settle().await;
    let ops = ops(&seen);
    assert!(ops.contains(&"suggestions_ask".to_string()));
    assert!(!ops.contains(&"ideas_list".to_string()), "{ops:?}");
    assert_eq!(locks(&seen).last(), Some(&("vet-2026-11-03".to_string(), false)));
}

#[tokio::test]
async fn a_refused_or_failed_send_mid_batch_still_sends_the_rest() {
    for knobs in [
        Knobs {
            answered: "b",
            ..Knobs::default()
        },
        Knobs {
            broken: "b",
            ..Knobs::default()
        },
    ] {
        let (base, seen) = store(knobs).await;
        let mut rig = live_rig(&base, true, true, &verdicts_json(&["a", "b", "c"]));
        minute(&mut rig).await;
        settle().await;
        assert_eq!(sent_ids(&seen), vec!["a", "b", "c"]);
    }
}

#[tokio::test]
async fn a_failed_session_sends_nothing() {
    let (base, seen) = store(Knobs::default()).await;
    let bad =
        r#"{"type":"result","subtype":"error_during_execution","is_error":true,"total_cost_usd":0.1,"session_id":"s"}"#;
    let mut rig = live_rig(&base, true, true, bad);
    minute(&mut rig).await;
    settle().await;
    assert!(sent_ids(&seen).is_empty());
    assert_eq!(locks(&seen).last(), Some(&("vet-2026-11-03".to_string(), false)));
    assert!(rig.lp(0).vet_daily.lock().unwrap().last.is_none());
}

#[tokio::test]
async fn the_same_day_isnt_run_twice() {
    let (base, seen) = store(Knobs::default()).await;
    let mut rig = live_rig(&base, true, true, &verdicts_json(&["a", "b", "c"]));
    minute(&mut rig).await;
    settle().await;
    rig.lp(0).vet_daily.lock().unwrap().forget_asker();
    minute(&mut rig).await;
    settle().await;
    assert_eq!(ops(&seen).iter().filter(|o| *o == "jobs_due").count(), 2);
    assert_eq!(sent_ids(&seen).len(), 3);
}
