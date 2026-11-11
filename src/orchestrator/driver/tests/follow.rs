//! Telling the chat how an approved draft moves on (`driver/follow.rs`, `driver/land.rs`): a fake store keeps the
//! chat, the items and what staging serves.
use super::*;
use crate::config::{DeployTarget, ProjectConfig};
use crate::drafts::DrafterResult;
use crate::implementer::publish::Published;
use crate::orchestrator::driver::{DraftHost, DraftJob};
use crate::test_support::{git, repo_with_origin, run};
use crate::view::DraftCommand;
use std::path::{Path, PathBuf};

struct Host;

impl DraftHost for Host {
    fn prepare(&self, _: &ProjectConfig, id: &str, _: &str) -> Result<PathBuf> {
        Ok(PathBuf::from(format!("draft-{id}")))
    }
    fn run(&self, _: DraftJob) -> BoxFuture<'static, Result<DrafterResult>> {
        async { anyhow::bail!("no drafter here") }.boxed()
    }
    fn publish(&self, _: &ProjectConfig, _: &Path, _: &str, _: &str, _: &str) -> Result<Published> {
        anyhow::bail!("no publish here")
    }
    fn remove(&self, _: &ProjectConfig, _: &str) {}
    fn merge(&self, _: &ProjectConfig, _: u64, _: bool) -> Result<()> {
        Ok(())
    }
    fn close(&self, _: &ProjectConfig, _: u64) -> Result<()> {
        Ok(())
    }
    fn rebase(&self, _: &ProjectConfig, _: &str, _: &str) -> Result<bool> {
        Ok(true)
    }
    fn push(&self, _: &ProjectConfig, _: &str, _: &str) -> Result<()> {
        Ok(())
    }
    fn fetch(&self, _: &ProjectConfig) -> Result<()> {
        Ok(())
    }
}

#[derive(Default, Clone)]
struct Chat {
    items: Arc<Mutex<Value>>,
    asks: Arc<Mutex<Vec<Value>>>,
    served: Arc<Mutex<String>>,
}

async fn store(chat: Chat) -> String {
    let c = chat.clone();
    let app = Router::new()
        .route(
            "/v1/repos/{id}/ops",
            post(move |Json(body): Json<Value>| {
                let chat = c.clone();
                async move {
                    let args = body["args"].clone();
                    let value = match body["op"].as_str().unwrap_or("") {
                        "list" => chat.items.lock().unwrap().clone(),
                        "asks_list" => json!({ "asks": chat.asks.lock().unwrap().clone() }),
                        "ask_report" if args["from"] == "retaskable" => {
                            let mut asks = chat.asks.lock().unwrap();
                            let id = format!("m{}", asks.len() + 1);
                            let mut ask = args.clone();
                            ask["id"] = json!(id);
                            asks.push(ask);
                            json!({ "id": id })
                        }
                        "draft_take" => json!({ "token": "lease-d", "draft": {
                            "id": "d1", "status": "needs_person", "detail": "awaiting_approval",
                            "titles": ["Alpha", "Beta"], "askId": "a1", "itemIds": ["a", "b"],
                            "prUrl": "https://github.com/o/r/pull/9", "branch": "retaskable/spec-d1" } }),
                        "upsert_from_specs" => json!({ "added": [] }),
                        _ => Value::Null,
                    };
                    Json(json!({ "value": value }))
                }
            }),
        )
        .route(
            "/version.json",
            axum::routing::get(move || {
                let served = chat.served.clone();
                async move { Json(json!({ "sha": served.lock().unwrap().clone() })) }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

fn set(chat: &Chat, a: &str, b: &str) {
    let item = |id: &str, title: &str, status: &str| json!({ "id": id, "title": title, "status": status, "rank": 1, "draftId": "d1" });
    *chat.items.lock().unwrap() = json!([item("a", "Alpha", a), item("b", "Beta", b)]);
}

/// Passes until the chat holds `n` messages (and a few more, to see that none follows).
async fn until(rig: &mut Rig, chat: &Chat, n: usize) -> Vec<Value> {
    for _ in 0..300 {
        rig.machine.pass().await;
        if chat.asks.lock().unwrap().len() >= n {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    for _ in 0..5 {
        rig.machine.pass().await;
    }
    chat.asks.lock().unwrap().clone()
}

fn texts(asks: &[Value]) -> Vec<&str> {
    asks.iter().map(|a| a["text"].as_str().unwrap()).collect()
}

#[tokio::test]
async fn an_approved_draft_tells_the_chat_how_it_moves_on_once_each() {
    let chat = Chat::default();
    set(&chat, "todo", "todo");
    let base = store(chat.clone()).await;
    let root = tempfile::tempdir().unwrap();
    let repo = repo_with_origin(root.path());
    run(git(&repo).args(["push", "--quiet", "origin", "main"]));
    let head = String::from_utf8(git(&repo).args(["rev-parse", "HEAD"]).output().unwrap().stdout).unwrap();
    let mut rig = rig(2, &[&base]);
    rig.go_live();
    rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(&["ask-claude"]));
    rig.lp(0).project.repo_path = repo.to_string_lossy().into_owned();
    rig.lp(0).project.base_branch = "main".into();
    rig.lp(0).project.deploy.staging = Some(DeployTarget {
        branch: "main".into(),
        url: Some(base.clone()),
        smoke: None,
        version_path: None,
        rollout: None,
    });
    rig.lp(0).seen.default_branch = Some("main".into());
    rig.machine.drafts = Some(Arc::new(Host));
    rig.machine.merge_wait = Duration::from_millis(1);
    rig.machine.links.item_commands.push_draft(DraftCommand::Approve {
        project: "p0".into(),
        draft: "d1".into(),
    });

    let asks = until(&mut rig, &chat, 1).await;
    assert_eq!(texts(&asks), ["Approved and merged. 2 items are queued: Alpha, Beta."]);
    assert_eq!(
        (asks[0]["draftId"].as_str(), asks[0]["replyTo"].as_str()),
        (Some("d1"), Some("a1"))
    );

    set(&chat, "building", "todo");
    let asks = until(&mut rig, &chat, 2).await;
    assert_eq!(texts(&asks)[1..], ["Started building Alpha."]);

    set(&chat, "on_main", "needs_person");
    let asks = until(&mut rig, &chat, 5).await;
    assert_eq!(
        texts(&asks)[2..],
        [
            "Alpha is built and merged.",
            "Started building Beta.",
            "Beta needs you. See its card."
        ]
    );

    // A restart forgets what it knew, and says nothing again.
    rig.lp(0).follow = Default::default();
    assert_eq!(until(&mut rig, &chat, 99).await.len(), 5);

    set(&chat, "on_main", "skipped");
    let asks = until(&mut rig, &chat, 6).await;
    assert_eq!(texts(&asks)[5], "Beta was skipped.");
    assert_eq!(asks.len(), 6, "staging serves nothing yet");

    *chat.served.lock().unwrap() = head.trim().to_string();
    rig.lp(0).follow.expedite();
    let asks = until(&mut rig, &chat, 7).await;
    assert_eq!(texts(&asks)[6], format!("It's on staging: {base}"));
    rig.lp(0).follow = Default::default();
    assert_eq!(until(&mut rig, &chat, 99).await.len(), 7);
    assert!(asks.iter().all(|a| a["draftId"] == "d1" && a["from"] == "retaskable"));
    assert!(asks[1..].iter().all(|a| a["replyTo"].is_string()));
}
