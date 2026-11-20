use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::discovery::scan;
use super::store::{KitStore, forget, kit_for};
use super::{Tier, read_kit};
use crate::backlog::Client;

type Seen = Arc<Mutex<Vec<(String, Value)>>>;

/// A store holding `held` as the `project_kit` fact, recording every op it is asked.
async fn store(held: Option<Value>) -> (Client, Seen) {
    let seen: Seen = Default::default();
    let log = seen.clone();
    let app = axum::Router::new().route(
        "/v1/repos/{id}/ops",
        axum::routing::post(move |axum::Json(req): axum::Json<Value>| {
            let (log, held) = (log.clone(), held.clone());
            async move {
                let op = req["op"].as_str().unwrap_or_default().to_string();
                log.lock().unwrap().push((op.clone(), req["args"].clone()));
                let value = match (op.as_str(), held) {
                    ("facts_get", Some(kit)) => json!({ "project_kit": { "data": kit, "updatedAt": 1 } }),
                    ("facts_get", None) => json!({}),
                    _ => json!({}),
                };
                axum::Json(json!({ "value": value }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (Client::new(base), seen)
}

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for c in ["Card", "Avatar", "Badge"] {
        let p = dir.path().join(format!("src/components/{c}.tsx"));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "").unwrap();
    }
    dir
}

type NoSession = fn(String) -> std::future::Ready<Option<Value>>;

fn no_session() -> Option<NoSession> {
    None
}

#[tokio::test]
async fn a_stored_kit_with_the_same_fingerprint_is_used_and_no_session_runs() {
    forget();
    let dir = project();
    let found = scan(dir.path());
    let mut kit = super::discovery::choose(&found);
    kit.pieces[0].name = "Stored name".into();
    let (client, seen) = store(Some(serde_json::to_value(&kit).unwrap())).await;
    let ks = KitStore {
        client: &client,
        repo: 1,
        token: "t",
        machine: "m",
    };
    let asked = Arc::new(Mutex::new(0));
    let a = asked.clone();
    let got = kit_for(
        dir.path(),
        Some(&ks),
        Some(move |_| async move {
            *a.lock().unwrap() += 1;
            None
        }),
        false,
    )
    .await;
    assert_eq!(got.pieces[0].name, "Stored name");
    assert_eq!(*asked.lock().unwrap(), 0);
    assert!(seen.lock().unwrap().iter().all(|(op, _)| op != "fact_set"));
}

#[tokio::test]
async fn a_changed_fingerprint_reads_again_runs_the_session_once_and_stores_the_kit() {
    forget();
    let dir = project();
    let mut old = super::discovery::choose(&scan(dir.path()));
    old.fingerprint = Some("ffffffffffffffff".into());
    let (client, seen) = store(Some(serde_json::to_value(&old).unwrap())).await;
    let ks = KitStore {
        client: &client,
        repo: 1,
        token: "t",
        machine: "laptop",
    };
    let tidy = json!({ "pieces": [{ "id": "card", "name": "Card", "kind": "card" }] });
    let got = kit_for(dir.path(), Some(&ks), Some(move |_| async move { Some(tidy) }), false).await;
    assert_eq!(got.pieces.len(), 1);
    let sent = seen.lock().unwrap().clone();
    let set = sent.iter().find(|(op, _)| op == "fact_set").expect("the kit is stored");
    assert_eq!(
        (set.1["kind"].as_str(), set.1["machine"].as_str()),
        (Some("project_kit"), Some("laptop"))
    );
    assert!(read_kit(&set.1["data"]).is_some());
    // Trusted for ten minutes: nothing is asked again.
    let before = sent.len();
    kit_for(dir.path(), Some(&ks), no_session(), false).await;
    assert_eq!(seen.lock().unwrap().len(), before);
}

#[tokio::test]
async fn an_unreachable_store_and_an_empty_folder_are_the_wireframe_kit_and_the_library_is_never_stored() {
    forget();
    let empty = tempfile::tempdir().unwrap();
    let down = Client::new("http://127.0.0.1:1");
    let ks = KitStore {
        client: &down,
        repo: 1,
        token: "t",
        machine: "m",
    };
    let got = kit_for(empty.path(), Some(&ks), no_session(), false).await;
    assert_eq!(got.tier, Tier::Wireframe);
    forget();
    let own = tempfile::tempdir().unwrap();
    for f in ["app/src/ui/whiteboardDesign.ts", "shared/designLayout.ts"] {
        let p = own.path().join(f);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "").unwrap();
    }
    let (client, seen) = store(None).await;
    let ks = KitStore {
        client: &client,
        repo: 1,
        token: "t",
        machine: "m",
    };
    assert_eq!(
        kit_for(own.path(), Some(&ks), no_session(), false).await.tier,
        Tier::Library
    );
    assert!(seen.lock().unwrap().iter().all(|(op, _)| op != "fact_set"));
}
