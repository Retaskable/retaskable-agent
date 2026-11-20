//! Keeps what the "Does this look right?" prompt shows on the plan, and so in the store with its details (TS `keepReview`,
//! add-and-design-review.md sections 6 and 7): the sentence (checked for jargon and length, sent back for one rewrite, else the
//! safe one) and the small validated layout, capped (`review`). Nothing the person wrote. It is written as the plan's
//! `designReview` through `draft_details_report`, a patch of that one field over the version read (a stale version reads again
//! and tries once more), while this computer still holds the plan: after it is let go its details are no one's to write.
use serde_json::{Map, Value, json};

use super::draw::{Drawn, Layouts};
use super::kit::catalog_of;
use super::layout::{ResolvedLayout, ResolvedPiece, check_layout};
use super::review::review_of_with;
use super::sentence::{How, settle_sentence};
use super::session::{Idea, sentence_prompt};
use crate::backlog::{BacklogError, Client};
use crate::kit::{Kit, review_kit_of};
use crate::text::truncate;

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
    /// The project's kit: the pieces the review uses go with it (`review.kit`).
    pub kit: &'a Kit,
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
    keep_layout(k, &drawn.layout, &text)
        .await
        .map(|kept| kept.then_some(how))
}

/// Builds the review for a checked layout and its settled sentence (with the kit's pieces it uses) and writes it. `false` when it
/// was too big for the review's cap even cut to one variation of one piece.
async fn keep_layout(k: &Keeping<'_>, layout: &ResolvedLayout, text: &str) -> Result<bool, BacklogError> {
    let used: Vec<String> = layout
        .variations
        .iter()
        .flat_map(|v| &v.pieces)
        .filter_map(|p| match p {
            ResolvedPiece::Library { element, .. } => Some(element.clone()),
            ResolvedPiece::New { .. } => None,
        })
        .collect();
    let kit = review_kit_of(k.kit, &used, layout.page.as_ref().map(|p| p.id.as_str()));
    let Some(review) = review_of_with(layout, text, kit.as_ref()) else {
        return Ok(false);
    };
    write_review(k, &review.to_string()).await?;
    Ok(true)
}

/// What came of a design the drafter gave with its plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanDesign {
    /// Checked and kept as the plan's `designReview`; how its sentence came out.
    Kept(How),
    /// Dropped, with why (a timeline line). Never repaired by a model.
    Dropped(String),
}

/// Keeps a design the drafter gave with its plan (`design`: the layout and one sentence), TS `keepPlanDesign`. Checked here in plain
/// code against the kit's closed sets; an invalid one is dropped with its reason, and a sentence that fails its check falls back to
/// the standard one, with no second model call.
pub async fn keep_plan_design(k: &Keeping<'_>, raw: &Value) -> Result<PlanDesign, BacklogError> {
    let mut layout_only = if raw.is_object() { raw.clone() } else { json!({}) };
    let sentence = layout_only
        .as_object_mut()
        .and_then(|o| o.remove("sentence"))
        .unwrap_or(Value::Null);
    let layout = match check_layout(&layout_only, &catalog_of(k.kit), &Default::default()) {
        Ok((layout, _)) => layout,
        Err(reason) => {
            return Ok(PlanDesign::Dropped(truncate(
                &format!("The drafter's design was dropped: {reason}"),
                280,
            )));
        }
    };
    let (text, how) = settle_sentence(&sentence, |_, _| async { None }).await;
    if keep_layout(k, &layout, &text).await? {
        Ok(PlanDesign::Kept(how))
    } else {
        Ok(PlanDesign::Dropped("The drafter's design was too big to keep.".into()))
    }
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

/// A send-back whose notes approve a variation ("Looks good", `pickedText`): the variation named is kept as `chosen` in the plan's stored
/// review, written like [`keep_review`] while this computer holds the plan. Answers the variation's name, or `None` when the notes
/// approve none or the plan holds no review. A store that can't be reached is the caller's to log.
pub async fn keep_chosen(
    client: &Client,
    repo: u64,
    token: &str,
    (draft_id, lease): (&str, &str),
    notes: &str,
) -> Result<Option<String>, BacklogError> {
    let mut tries = 0;
    loop {
        let record = client.draft_details_get(repo, Some(token), draft_id).await?;
        let Some(held) = record.details.get("designReview").and_then(Value::as_str) else {
            return Ok(None);
        };
        let Ok(mut review) = serde_json::from_str::<Value>(held) else {
            return Ok(None);
        };
        let Some(at) = super::review::chosen_in(&review, notes) else {
            return Ok(None);
        };
        if review["chosen"] == json!(at) {
            return Ok(None);
        }
        let name = review["variations"][at]["name"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        review["chosen"] = json!(at);
        let mut patch = Map::new();
        patch.insert("designReview".into(), json!(review.to_string()));
        let sent = client
            .draft_details_report(repo, Some(token), (draft_id, lease), record.version, &patch)
            .await;
        match sent {
            Err(e) if e.code() == Some("conflict") && tries < 1 => tries += 1,
            done => return done.map(|_| Some(name)),
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
            kit: crate::kit::library_ref(),
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

    #[tokio::test]
    async fn an_approved_variation_is_kept_as_chosen_and_other_notes_and_a_plan_with_no_review_keep_nothing() {
        let review = r#"{"sentence":"s","variations":[{"name":"Side by side","pieces":[{"kind":"library","element":"card"}]},{"name":"Stacked","pieces":[{"kind":"library","element":"card"}]}]}"#;
        let notes = "The design is approved. Plan it as shown: \"Stacked\" as a new screen, made of card.";
        let (client, seen) = store(Some(review.into()), 0).await;
        let name = keep_chosen(&client, 1, "t", ("d1", "lease"), notes).await.unwrap();
        assert_eq!(name.as_deref(), Some("Stacked"));
        let sent = seen.lock().unwrap().clone();
        let patch = sent.last().unwrap()["args"]["patch"]["designReview"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(serde_json::from_str::<Value>(&patch).unwrap()["chosen"], 1);
        let (client, seen) = store(Some(review.into()), 0).await;
        assert!(
            keep_chosen(&client, 1, "t", ("d1", "lease"), "Make it bigger")
                .await
                .unwrap()
                .is_none()
        );
        assert!(seen.lock().unwrap().iter().all(|b| b["op"] != "draft_details_report"));
        let (client, _) = store(None, 0).await;
        assert!(
            keep_chosen(&client, 1, "t", ("d1", "lease"), notes)
                .await
                .unwrap()
                .is_none()
        );
    }
}
