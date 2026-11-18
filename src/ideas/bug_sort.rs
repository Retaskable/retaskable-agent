//! A bug typed as an idea moves to Bugs (docs/specs/questions-and-triage.md, the Bugs spec section 5,
//! `ideaBugSort.ts`): when this computer first claims the plan queued for a person's idea, the idea's text is classified
//! once and the raw `{kind, confidence}` is sent (`idea_classify`, pools-on-server.md section 18.3). The store holds the threshold
//! and moves a bug at or above it to Bugs, so its plan is written as a fix. Anything else, or any error, leaves it an idea; it
//! never blocks the draft.
use std::future::Future;

use crate::ask::classify::Classified;
use crate::backlog::drafts::ClaimedDraft;

use super::accept::person_request;
use super::store::{IdeaState, Ideas, StoreIdea};

/// The idea a draft is the plan of, when it is a waiting person's idea with no bug mark and no pictures.
pub fn sortable<'a>(ideas: &'a [StoreIdea], draft_id: &str) -> Option<&'a StoreIdea> {
    ideas.iter().find(|i| {
        i.state == IdeaState::Waiting
            && i.added_by.is_some()
            && i.plan_id.as_deref() == Some(draft_id)
            && !i.bug
            && i.pictures.is_empty()
    })
}

/// A first claim: not a hand-off, a move, an answer or a send-back.
pub fn first_claim(draft: &ClaimedDraft) -> bool {
    let set = |k: &str| draft.other.get(k).is_some_and(|v| !v.is_null() && v != false);
    draft.intake.is_some() && draft.answer.is_none() && !["movedFrom", "handedOff", "revise"].iter().any(|k| set(k))
}

/// The text the classifier reads: the idea's title and what the person said they'd see.
fn text_of(idea: &StoreIdea) -> String {
    let experience = idea.idea.as_ref().map_or("", |b| b.experience.as_str());
    format!("{}\n\n{experience}", idea.title).trim().to_string()
}

/// Called with a draft just claimed from the store's queue. Sends the classification of the idea's text; when the store moved the
/// idea to Bugs, rewrites the claimed copy's request as a fix and answers true. `classify` gets the idea's text.
pub async fn sort<F, Fut>(ideas: &Ideas<'_>, draft: &mut ClaimedDraft, classify: F) -> bool
where
    F: FnOnce(String) -> Fut,
    Fut: Future<Output = Classified>,
{
    if !first_claim(draft) {
        return false;
    }
    let list = match ideas.list().await {
        Ok(list) => list,
        Err(e) => return left(&draft.id, &e.to_string()),
    };
    let Some(idea) = sortable(&list.ideas, &draft.id) else {
        return false;
    };
    // A classifier that could not run reports nothing: the idea stays an idea.
    let Classified::Sorted(result) = classify(text_of(idea)).await else {
        tracing::info!("ideas: {} stays an idea (no classification)", idea.id);
        return false;
    };
    match ideas.classify(&idea.id, result.kind.name(), result.confidence).await {
        Ok(answer) if answer.moved => {}
        Ok(answer) => {
            tracing::info!(
                "ideas: {} stays an idea ({} at {}, the store moves a bug at {}%)",
                idea.id,
                result.kind.name(),
                result.confidence,
                answer.threshold
            );
            return false;
        }
        Err(e) => return left(&draft.id, &e.to_string()),
    }
    let who = idea
        .added_by
        .as_ref()
        .and_then(|a| a.get("login")?.as_str())
        .unwrap_or_default();
    let experience = idea.idea.as_ref().map_or("", |b| b.experience.as_str());
    draft.request = Some(person_request(&idea.title, experience, who, true));
    if let Some(intake) = draft.intake.as_mut() {
        intake.kinds = vec!["fix".to_string()];
    }
    tracing::info!("ideas: {} moved to Bugs", idea.id);
    true
}

fn left(draft: &str, why: &str) -> bool {
    tracing::info!("ideas: {draft} left as an idea: {why}");
    false
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::ask::classify::{Kind, Sorted};

    fn idea(plan: &str) -> StoreIdea {
        serde_json::from_value(json!({
            "id": "i1", "title": "Save button dead", "state": "waiting", "at": 0,
            "idea": { "experience": "Nothing happens" },
            "addedBy": { "login": "ann", "at": 1 }, "planId": plan
        }))
        .unwrap()
    }

    fn draft() -> ClaimedDraft {
        serde_json::from_value(json!({ "id": "d1", "request": "old", "intake": { "kinds": ["feature"] } })).unwrap()
    }

    fn sorted(kind: Kind, confidence: f64) -> Classified {
        Classified::Sorted(Sorted {
            kind,
            confidence,
            title: None,
        })
    }

    #[test]
    fn only_a_waiting_persons_own_plain_idea_is_sortable() {
        let i = idea("d1");
        assert!(sortable(std::slice::from_ref(&i), "d1").is_some());
        assert!(sortable(std::slice::from_ref(&i), "other").is_none());
        let pic = StoreIdea {
            pictures: vec!["a.png".into()],
            ..idea("d1")
        };
        assert!(sortable(&[pic], "d1").is_none());
        let marked = StoreIdea {
            bug: true,
            ..idea("d1")
        };
        assert!(sortable(&[marked], "d1").is_none());
    }

    #[test]
    fn a_first_claim_is_not_a_hand_off_a_move_an_answer_or_a_send_back() {
        assert!(first_claim(&draft()));
        for k in ["movedFrom", "handedOff", "revise"] {
            let mut d = draft();
            d.other.insert(k.into(), json!("x"));
            assert!(!first_claim(&d), "{k}");
        }
        let mut d = draft();
        d.answer = Some(crate::backlog::drafts::DraftAnswer {
            answers: vec![],
            by: None,
        });
        assert!(!first_claim(&d));
        d.answer = None;
        d.intake = None;
        assert!(!first_claim(&d));
    }

    /// A store answering `ideas_list` with `list` and `idea_classify` as the real one does for a threshold of `pct` (it moves a bug at
    /// or above it); every classification it is sent is kept.
    async fn store(
        sent: std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
        list: serde_json::Value,
        pct: u64,
    ) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = axum::Router::new().route(
            "/v1/repos/{id}/ops",
            axum::routing::post(move |axum::Json(req): axum::Json<serde_json::Value>| {
                let (sent, list) = (sent.clone(), list.clone());
                async move {
                    if req["op"] == "idea_classify" {
                        let args = req["args"].clone();
                        let moved = args["kind"] == "bug"
                            && (args["confidence"].as_f64().unwrap() * 100.0).round() >= pct as f64;
                        sent.lock().unwrap().push(args);
                        return axum::Json(json!({ "value": { "moved": moved, "threshold": pct } }));
                    }
                    axum::Json(json!({ "value": { "round": null, "ideas": list } }))
                }
            }),
        );
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    async fn run_at(
        list: serde_json::Value,
        result: Classified,
        pct: u64,
    ) -> (bool, ClaimedDraft, Vec<serde_json::Value>) {
        let sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let base = store(sent.clone(), list, pct).await;
        let client = crate::backlog::Client::new(&base);
        let ideas = Ideas {
            client: &client,
            repo_id: 1,
            token: None,
        };
        let mut d = draft();
        let moved = sort(&ideas, &mut d, |_| async move { result }).await;
        let sent = sent.lock().unwrap().clone();
        (moved, d, sent)
    }

    async fn run(list: serde_json::Value, result: Classified) -> (bool, ClaimedDraft, Vec<serde_json::Value>) {
        run_at(list, result, 80).await
    }

    fn row(i: &StoreIdea) -> serde_json::Value {
        json!({ "id": i.id, "title": i.title, "state": "waiting", "at": 0, "planId": i.plan_id,
            "addedBy": { "login": "ann", "at": 1 }, "idea": { "experience": "Nothing happens" },
            "pictures": i.pictures })
    }

    #[tokio::test]
    async fn the_raw_classification_is_sent_and_a_bug_the_store_moves_becomes_a_fix() {
        let (moved, d, sent) = run(json!([row(&idea("d1"))]), sorted(Kind::Bug, 0.9)).await;
        assert_eq!(sent, [json!({ "id": "i1", "kind": "bug", "confidence": 0.9 })]);
        assert!(moved);
        assert_eq!(d.intake.unwrap().kinds, ["fix"]);
        assert!(d.request.unwrap().contains("Plan it as a fix"));
    }

    #[tokio::test]
    async fn the_threshold_is_the_stores_not_the_agents() {
        let plain = row(&idea("d1"));
        // 0.79 would have stayed an idea under the old local 0.8; the store holding 75 moves it, and one holding 95 leaves 0.9.
        let (moved, _, sent) = run_at(json!([plain.clone()]), sorted(Kind::Bug, 0.79), 75).await;
        assert!(moved && sent.len() == 1);
        let (moved, d, sent) = run_at(json!([plain]), sorted(Kind::Bug, 0.9), 95).await;
        assert!(!moved && sent.len() == 1, "still sent: the store decides");
        assert_eq!(d.request.as_deref(), Some("old"));
    }

    #[tokio::test]
    async fn an_idea_is_reported_too_and_a_failed_classifier_and_a_picture_bearing_idea_send_nothing() {
        let plain = row(&idea("d1"));
        let (moved, d, sent) = run(json!([plain.clone()]), sorted(Kind::Idea, 0.95)).await;
        assert!(!moved && d.request.as_deref() == Some("old"));
        assert_eq!(sent, [json!({ "id": "i1", "kind": "idea", "confidence": 0.95 })]);
        let (moved, d, sent) = run(json!([plain.clone()]), Classified::Unavailable).await;
        assert!(!moved && sent.is_empty() && d.request.as_deref() == Some("old"));
        let mut pic = plain.clone();
        pic["pictures"] = json!(["a.png"]);
        let (moved, _, sent) = run(json!([pic]), sorted(Kind::Bug, 1.0)).await;
        assert!(!moved && sent.is_empty());
    }
}
