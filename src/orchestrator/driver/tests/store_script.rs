//! A fake store that logs every op, for the driver tests that run real git and no session (`main_sync.rs`, `sync_now.rs`).
use super::*;
use crate::backlog::runs::RunKind;
use axum::http::StatusCode;

#[derive(Default)]
pub(super) struct Script {
    pub(super) log: Vec<Value>,
    pub(super) items: Vec<Value>,
    pub(super) lock: Option<Value>,
    pub(super) list_fails: bool,
    pub(super) lock_fails: bool,
    /// What `chore_trigger` answers (queued when unset), and whether it breaks.
    pub(super) trigger: Option<Value>,
    pub(super) trigger_fails: bool,
}

pub(super) type Store_ = Arc<Mutex<Script>>;

impl Script {
    pub(super) fn args(&self, op: &str) -> Vec<Value> {
        self.log
            .iter()
            .filter(|b| b["op"] == op)
            .map(|b| b["args"].clone())
            .collect()
    }
}

pub(super) async fn store() -> (String, Store_) {
    let script: Store_ = Arc::default();
    let app = Router::new()
        .route(
            "/v1/repos/{id}/ops",
            post(
                |axum::extract::State(s): axum::extract::State<Store_>, Json(body): Json<Value>| async move {
                    let mut s = s.lock().unwrap();
                    s.log.push(body.clone());
                    let ok = |v: Value| (StatusCode::OK, Json(json!({ "value": v })));
                    match body["op"].as_str().unwrap() {
                        "list" if s.list_fails => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": "boom" }))),
                        "list" => ok(Value::Array(s.items.clone())),
                        "chore_trigger" if s.trigger_fails => {
                            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": "boom" })))
                        }
                        "chore_trigger" => ok(s.trigger.clone().unwrap_or(json!({ "queued": "chore:fix_main" }))),
                        "lock_take" if s.lock_fails => {
                            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": "boom" })))
                        }
                        "lock_take" => ok(s.lock.clone().unwrap_or(json!({ "held": true, "token": "L1" }))),
                        _ => ok(Value::Null),
                    }
                },
            ),
        )
        .with_state(script.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), script)
}

pub(super) struct Idle;
impl RunStarter for Idle {
    fn kinds(&self) -> Vec<RunKind> {
        vec![RunKind::Build]
    }
    fn start(&self, _: RunRequest) -> Result<(), String> {
        Err("nothing to start".into())
    }
}
