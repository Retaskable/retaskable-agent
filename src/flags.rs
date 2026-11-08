//! The Retaskable flags client (docs/specs/release-flags.md section 3.5), in Rust: `GET <base>/api/flags/<project>`
//! with `Authorization: Bearer <key>`. A flag is Off until there is an answer, and a request that fails keeps the
//! last answer. No key (`RETASKABLE_FLAG_KEY` unset) means no request and every flag Off.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// This project's numeric id: the owner fills it in from Settings → Project settings → Release flags.
pub const PROJECT_ID: &str = "PROJECT_ID_PLACEHOLDER";
const BASE: &str = "https://retaskable.com";
const KEY_VAR: &str = "RETASKABLE_FLAG_KEY";
const DEFAULT_TTL: Duration = Duration::from_secs(300);
const TIMEOUT: Duration = Duration::from_secs(10);

struct State {
    flags: BTreeMap<String, bool>,
    ttl: Duration,
    fetched: Option<Instant>,
}

pub struct Flags {
    base: String,
    project: String,
    key: String,
    http: reqwest::Client,
    state: Mutex<State>,
}

impl Flags {
    pub fn new(base: &str, project: &str, key: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            project: project.to_string(),
            key: key.to_string(),
            http: reqwest::Client::builder()
                .timeout(TIMEOUT)
                .build()
                .expect("building the flags HTTP client"),
            state: Mutex::new(State {
                flags: BTreeMap::new(),
                ttl: DEFAULT_TTL,
                fetched: None,
            }),
        }
    }

    /// This project's client, keyed from the environment.
    pub fn from_env() -> Self {
        Self::new(BASE, PROJECT_ID, &std::env::var(KEY_VAR).unwrap_or_default())
    }

    /// A client that never asks and answers `on` for exactly these flags.
    pub fn fixed(on: &[&str]) -> Self {
        let flags = Self::new(BASE, PROJECT_ID, "");
        flags.state.lock().unwrap().flags = on.iter().map(|k| (k.to_string(), true)).collect();
        flags
    }

    /// Whether `name` is On, asking again first when the last answer is older than its ttl.
    pub async fn on(&self, name: &str) -> bool {
        self.refresh_if_stale().await;
        self.state.lock().unwrap().flags.get(name) == Some(&true)
    }

    /// Whether `name` was On at the last answer, without asking again: Off until there has been one.
    pub fn last(&self, name: &str) -> bool {
        self.state.lock().unwrap().flags.get(name) == Some(&true)
    }

    async fn refresh_if_stale(&self) {
        if self.key.is_empty() {
            return;
        }
        {
            let s = self.state.lock().unwrap();
            if s.fetched.is_some_and(|at| at.elapsed() < s.ttl) {
                return;
            }
        }
        // A failed request keeps the last answer.
        let Some((flags, ttl)) = self.fetch().await else { return };
        *self.state.lock().unwrap() = State {
            flags,
            ttl,
            fetched: Some(Instant::now()),
        };
    }

    async fn fetch(&self) -> Option<(BTreeMap<String, bool>, Duration)> {
        let url = format!("{}/api/flags/{}", self.base, self.project);
        let res = self.http.get(url).bearer_auth(&self.key).send().await.ok()?;
        if !res.status().is_success() {
            return None;
        }
        let body: serde_json::Value = res.json().await.ok()?;
        let flags = body
            .get("flags")?
            .as_object()?
            .iter()
            .map(|(k, v)| (k.clone(), v.as_bool() == Some(true)))
            .collect();
        let ttl = body
            .get("ttl")
            .and_then(|t| t.as_f64())
            .filter(|t| *t > 0.0)
            .map_or(DEFAULT_TTL, Duration::from_secs_f64);
        Some((flags, ttl))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, http::StatusCode, routing::get};
    use serde_json::json;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    async fn serve(ok: Arc<AtomicBool>) -> String {
        let app = Router::new().route(
            "/api/flags/{project}",
            get(move |headers: axum::http::HeaderMap| {
                let ok = ok.clone();
                async move {
                    let authed = headers.get("authorization").is_some_and(|v| v == "Bearer k");
                    if ok.load(Ordering::SeqCst) && authed {
                        (
                            StatusCode::OK,
                            Json(json!({ "flags": { "ask-claude": true, "x": false }, "ttl": 0.05 })),
                        )
                    } else {
                        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({})))
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn a_flag_is_off_until_answered_and_the_last_answer_survives_a_failure() {
        let ok = Arc::new(AtomicBool::new(false));
        let base = serve(ok.clone()).await;
        let flags = Flags::new(&base, "7", "k");
        assert!(!flags.on("ask-claude").await);
        ok.store(true, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(flags.on("ask-claude").await);
        assert!(!flags.on("x").await);
        ok.store(false, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert!(flags.on("ask-claude").await);
    }

    #[tokio::test]
    async fn no_key_means_every_flag_is_off() {
        assert!(!Flags::new("http://127.0.0.1:1", "7", "").on("ask-claude").await);
    }
}
