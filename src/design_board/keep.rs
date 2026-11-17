//! Keeps what the "Does this look right?" prompt shows on the plan, and so in the store with its details (TS `keepReview`,
//! add-and-design-review.md sections 6 and 7): the sentence (checked for jargon and length, sent back for one rewrite, else the
//! safe one) and the small validated layout, capped (`review`). Nothing the person wrote. It is written as the plan's
//! `designReview` through `draft_details_report`, a patch of that one field over the version read (a stale version reads again
//! and tries once more), while this computer still holds the plan: after it is let go its details are no one's to write.
use serde_json::{Map, Value, json};

use super::draw::{Drawn, Layouts};
use super::review::review_of;
use super::sentence::{How, settle_sentence};
use super::session::{Idea, sentence_prompt};
use crate::backlog::{BacklogError, Client};

/// Everything keeping a review reaches: the store, the plan and the lease this computer holds it by, and the session that
/// rewrites a sentence that failed its check.
pub struct Keeping<'a> {
    pub client: &'a Client,
    pub repo: u64,
    /// This computer's access token for the store.
    pub token: &'a str,
    pub draft_id: &'a str,
    /// The plan's lease.
    pub lease: &'a str,
    pub sentences: Layouts<'a>,
}

/// Settles the sentence, builds the review and writes it into the plan's details. `Ok(None)` when there was nothing to keep (a
/// layout too big for the review's cap even cut to one variation of one piece); the sentence's way otherwise.
pub async fn keep_review(k: &Keeping<'_>, idea: &Idea<'_>, drawn: &Drawn) -> Result<Option<How>, BacklogError> {
    let (text, how) = settle_sentence(&drawn.sentence, |problems, was| async move {
        let prompt = sentence_prompt(&problems, &was, idea).ok()?;
        let answered = (k.sentences)(prompt).await.ok()?;
        answered.get("sentence").cloned()
    })
    .await;
    let Some(review) = review_of(&drawn.layout, &text) else {
        return Ok(None);
    };
    write_review(k, &review.to_string()).await?;
    Ok(Some(how))
}

/// Writes `designReview` (the review's JSON as text) unless the store already holds it, reading the record again after a stale
/// version, once.
async fn write_review(k: &Keeping<'_>, review: &str) -> Result<(), BacklogError> {
    let mut tries = 0;
    loop {
        let record = k.client.draft_details_get(k.repo, Some(k.token), k.draft_id).await?;
        if record.details.get("designReview").and_then(Value::as_str) == Some(review) {
            return Ok(());
        }
        let mut patch = Map::new();
        patch.insert("designReview".into(), json!(review));
        let sent = k
            .client
            .draft_details_report(k.repo, Some(k.token), (k.draft_id, k.lease), record.version, &patch)
            .await;
        match sent {
            Err(e) if e.code() == Some("conflict") && tries < 1 => tries += 1,
            done => return done.map(|_| ()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::{Json, Router, extract::State, routing::post};
    use futures_util::future::BoxFuture;

    use super::*;
    use crate::design_board::error::DesignError;
    use crate::design_board::layout::{ResolvedLayout, ResolvedPiece, ResolvedVariation};

    const GOOD: &str = "You'll see a snooze button on each alert, so you can pause it.";

    /// A store whose draft details are at `version`, held in `held`; the first `stale` reports answer `conflict` and move it on.
    async fn store(held: Option<String>, stale: usize) -> (Client, Arc<Mutex<Vec<Value>>>) {
        struct Store {
            seen: Arc<Mutex<Vec<Value>>>,
            version: Mutex<u64>,
            held: Option<String>,
            stale: Mutex<usize>,
        }
        async fn ops(State(s): State<Arc<Store>>, Json(body): Json<Value>) -> (axum::http::StatusCode, Json<Value>) {
            s.seen.lock().unwrap().push(body.clone());
            let version = *s.version.lock().unwrap();
            let ok = |v: Value| (axum::http::StatusCode::OK, Json(json!({ "value": v })));
            match body["op"].as_str() {
                Some("draft_details_get") => ok(json!({ "version": version, "details": match &s.held {
                    Some(r) => json!({ "designReview": r }),
                    None => json!({}),
                } })),
                Some("draft_details_report") => {
                    let mut stale = s.stale.lock().unwrap();
                    if *stale > 0 || body["args"]["baseVersion"] != version {
                        *stale = stale.saturating_sub(1);
                        *s.version.lock().unwrap() += 1;
                        return (
                            axum::http::StatusCode::CONFLICT,
                            Json(json!({ "error": "conflict", "message": "changed" })),
                        );
                    }
                    ok(json!({ "version": version + 1, "details": body["args"]["patch"] }))
                }
                _ => ok(Value::Null),
            }
        }
        let seen: Arc<Mutex<Vec<Value>>> = Arc::default();
        let app = Router::new()
            .route("/v1/repos/{id}/ops", post(ops))
            .with_state(Arc::new(Store {
                seen: seen.clone(),
                version: Mutex::new(5),
                held,
                stale: Mutex::new(stale),
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (Client::new(base), seen)
    }

    fn drawn(sentence: Value) -> Drawn {
        Drawn {
            layout: ResolvedLayout {
                page: None,
                variations: vec![ResolvedVariation {
                    name: "A".into(),
                    pieces: vec![ResolvedPiece::Library {
                        element: "card".into(),
                        label: None,
                    }],
                }],
                replaces: vec![],
            },
            sentence,
        }
    }

    fn idea() -> Idea<'static> {
        Idea {
            title: "Snooze",
            request: "Let people snooze",
            summary: None,
            change: None,
        }
    }

    fn rewriting(
        answer: Result<Value, DesignError>,
    ) -> impl Fn(String) -> BoxFuture<'static, Result<Value, DesignError>> + Send + Sync {
        move |_| {
            let answer = answer.clone();
            Box::pin(async move { answer })
        }
    }

    fn keeping<'a>(client: &'a Client, sentences: Layouts<'a>) -> Keeping<'a> {
        Keeping {
            client,
            repo: 7,
            token: "tok",
            draft_id: "draft-1",
            lease: "lease-1",
            sentences,
        }
    }

    fn reports(seen: &Arc<Mutex<Vec<Value>>>) -> Vec<Value> {
        seen.lock()
            .unwrap()
            .iter()
            .filter(|b| b["op"] == "draft_details_report")
            .map(|b| b["args"].clone())
            .collect()
    }

    #[tokio::test]
    async fn a_stale_version_reads_the_record_again_and_tries_once_more() {
        let (client, seen) = store(None, 1).await;
        let rewrite = rewriting(Err(DesignError::conflict("no")));
        let how = keep_review(&keeping(&client, &rewrite), &idea(), &drawn(json!(GOOD)))
            .await
            .unwrap();
        assert_eq!(how, Some(How::First));
        let sent = reports(&seen);
        assert_eq!(
            sent.iter()
                .map(|a| a["baseVersion"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            [5, 6]
        );
        assert!(sent[1]["patch"]["designReview"].as_str().unwrap().contains(GOOD));
        assert_eq!(sent[1]["token"], "lease-1");
    }

    #[tokio::test]
    async fn a_second_conflict_is_the_error_and_nothing_more_is_sent() {
        let (client, seen) = store(None, 2).await;
        let rewrite = rewriting(Err(DesignError::conflict("no")));
        let err = keep_review(&keeping(&client, &rewrite), &idea(), &drawn(json!(GOOD)))
            .await
            .unwrap_err();
        assert_eq!(err.code(), Some("conflict"));
        assert_eq!(reports(&seen).len(), 2);
    }

    #[tokio::test]
    async fn a_review_the_store_already_holds_is_not_written_again() {
        let held = crate::design_board::review::review_of(&drawn(json!(GOOD)).layout, GOOD)
            .unwrap()
            .to_string();
        let (client, seen) = store(Some(held), 0).await;
        let rewrite = rewriting(Err(DesignError::conflict("no")));
        keep_review(&keeping(&client, &rewrite), &idea(), &drawn(json!(GOOD)))
            .await
            .unwrap();
        assert!(reports(&seen).is_empty());
    }

    #[tokio::test]
    async fn a_layout_too_big_for_the_cap_keeps_nothing_and_writes_nothing() {
        let (client, seen) = store(None, 0).await;
        let rewrite = rewriting(Err(DesignError::conflict("no")));
        let mut big = drawn(json!(GOOD));
        big.layout.variations[0].pieces = vec![ResolvedPiece::New {
            name: "x".into(),
            purpose: "y".repeat(8000),
        }];
        assert_eq!(
            keep_review(&keeping(&client, &rewrite), &idea(), &big).await.unwrap(),
            None
        );
        assert!(reports(&seen).is_empty());
    }
}
