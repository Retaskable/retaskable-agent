//! The fake backlog store the expert tests share: a stateful stand-in for the add, queue, lease and ship ops of a real
//! one, recording each op it is asked.
use super::*;
use axum::http::StatusCode;
use std::collections::BTreeSet;

#[derive(Default)]
pub(super) struct St {
    pub(super) ops: Asked,
    pub(super) rows: Mutex<Vec<Value>>,
    /// The continuation notes `expert_continues_set` wrote, by item.
    pub(super) notes: Mutex<serde_json::Map<String, Value>>,
    /// Ids `add` refuses as taken.
    pub(super) taken_ids: Mutex<BTreeSet<String>>,
    pub(super) seq: AtomicU32,
    /// What `draft_queue` and `release` are refused with (a 4xx code), while set.
    pub(super) queue_refused: AtomicBool,
    pub(super) release_refused: Mutex<Option<&'static str>>,
    /// What `expert_ship` is refused with (a store code, or `down` for a 5xx), while set; and whether the next one is
    /// taken but its reply is lost.
    pub(super) ship_op_refused: Mutex<Option<&'static str>>,
    pub(super) ship_reply_lost: AtomicBool,
}

impl St {
    pub(super) fn handle(&self, body: Value) -> (StatusCode, Json<Value>) {
        let op = body["op"].as_str().unwrap_or("").to_string();
        let args = body["args"].clone();
        self.ops.lock().unwrap().push((op.clone(), args.clone()));
        let ok = |v: Value| (StatusCode::OK, Json(json!({ "value": v })));
        let conflict = || {
            (
                StatusCode::CONFLICT,
                Json(json!({ "error": "conflict", "message": "taken" })),
            )
        };
        let item = args["itemId"].as_str().unwrap_or("").to_string();
        let mut rows = self.rows.lock().unwrap();
        match op.as_str() {
            "list" => ok(json!([])),
            "facts_get" => ok(json!({ "paused": false })),
            "add" => {
                let it = &args["items"][0];
                let id = it["id"].as_str().unwrap_or("").to_string();
                if !self.taken_ids.lock().unwrap().insert(id.clone()) {
                    return conflict();
                }
                let item = json!({ "id": id, "title": it["title"], "status": "claimed", "rank": 1 });
                ok(json!({ "item": item, "token": format!("lease-{id}") }))
            }
            "expert_queue_add" => {
                let n = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
                let at = crate::telemetry::now_ms() + i64::from(n);
                let id = format!("q-{n}");
                rows.push(json!({
                    "itemId": item, "id": id, "text": args["text"], "at": at, "by": args["by"], "state": "queued",
                    "pictureIds": args.get("pictureIds").cloned().unwrap_or(json!([]))
                }));
                ok(json!({ "id": id }))
            }
            "expert_queue_take" => {
                let ids: Vec<String> = serde_json::from_value(args["ids"].clone()).unwrap_or_default();
                let mut got = vec![];
                for id in ids {
                    match rows.iter_mut().find(|r| r["id"] == id && r["itemId"] == item) {
                        Some(r) => {
                            r["state"] = json!("taken");
                            got.push(r.clone());
                        }
                        None => return conflict(),
                    }
                }
                ok(json!({ "rows": got }))
            }
            "expert_queue_done" => {
                let ids: Vec<Value> = args["ids"].as_array().cloned().unwrap_or_default();
                rows.retain(|r| !(r["itemId"] == item && ids.contains(&r["id"])));
                ok(json!({ "ok": true }))
            }
            "expert_continues_set" => {
                self.notes.lock().unwrap().insert(item, args["from"].clone());
                ok(Value::Null)
            }
            "draft_queue" if self.queue_refused.load(Ordering::SeqCst) => conflict(),
            "draft_queue" => ok(json!({ "draft": { "id": "draft-1" } })),
            "release" if self.release_refused.lock().unwrap().is_some() => {
                let code = self.release_refused.lock().unwrap().unwrap();
                let status = if code == "stale_lease" {
                    StatusCode::CONFLICT
                } else {
                    StatusCode::INTERNAL_SERVER_ERROR
                };
                (status, Json(json!({ "error": code, "message": code })))
            }
            "expert_ship" => {
                let refused = *self.ship_op_refused.lock().unwrap();
                let lost = self.ship_reply_lost.swap(false, Ordering::SeqCst);
                let status = match refused {
                    Some("not_found") => StatusCode::NOT_FOUND,
                    Some("bad_request") => StatusCode::BAD_REQUEST,
                    Some("forbidden") => StatusCode::FORBIDDEN,
                    Some("conflict" | "stale_lease") => StatusCode::CONFLICT,
                    Some(_) => StatusCode::INTERNAL_SERVER_ERROR,
                    None if lost => StatusCode::INTERNAL_SERVER_ERROR,
                    None => {
                        return ok(
                            json!({ "piece": format!("{}#1", args["id"].as_str().unwrap_or("")), "pieces": [] }),
                        );
                    }
                };
                let code = refused.unwrap_or("down");
                (status, Json(json!({ "error": code, "message": code })))
            }
            "revise" => ok(json!({ "revised": "applied", "status": "todo" })),
            "expert_queue_clear" => {
                rows.retain(|r| r["itemId"] != item);
                ok(Value::Null)
            }
            "expert_queue_list" => {
                let mut sessions = serde_json::Map::new();
                for r in rows.iter() {
                    let k = r["itemId"].as_str().unwrap_or("").to_string();
                    sessions
                        .entry(k)
                        .or_insert_with(|| json!([]))
                        .as_array_mut()
                        .unwrap()
                        .push(r.clone());
                }
                ok(json!({ "sessions": sessions, "continues": *self.notes.lock().unwrap() }))
            }
            _ => ok(Value::Null),
        }
    }

    pub(super) fn count(&self, op: &str) -> usize {
        self.ops.lock().unwrap().iter().filter(|(o, _)| o == op).count()
    }

    pub(super) fn args(&self, op: &str) -> Vec<Value> {
        (self.ops.lock().unwrap().iter())
            .filter(|(o, _)| o == op)
            .map(|(_, a)| a.clone())
            .collect()
    }

    /// A message the site wrote, which this computer didn't.
    pub(super) fn site_says(&self, item: &str, text: &str) {
        let n = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        self.rows.lock().unwrap().push(json!({
            "itemId": item, "id": format!("q-{n}"), "text": text,
            "at": crate::telemetry::now_ms() + i64::from(n), "by": "ada", "state": "queued"
        }));
    }

    pub(super) fn cancel(&self, id: &str) {
        self.rows.lock().unwrap().retain(|r| r["id"] != id);
    }
}

pub(super) async fn store() -> (String, Arc<St>) {
    let st = Arc::new(St::default());
    let s = st.clone();
    let app = Router::new()
        .route(
            "/v1/repos/{id}/ops",
            post(move |Json(body): Json<Value>| {
                let s = s.clone();
                async move { s.handle(body) }
            }),
        )
        // Every picture the store is asked for is its own name, but `0…ff`, which it doesn't have.
        .route(
            "/v1/repos/{id}/pictures/{picture}",
            axum::routing::get(
                |axum::extract::Path((_, picture)): axum::extract::Path<(u64, String)>| async move {
                    if picture.contains("-0000-4000-8000-0000000000ff") {
                        (StatusCode::NOT_FOUND, b"{}".to_vec())
                    } else {
                        (StatusCode::OK, picture.into_bytes())
                    }
                },
            ),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), st)
}
