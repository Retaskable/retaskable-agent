//! A bug typed as an idea moves to Bugs (docs/specs/questions-and-triage.md, the Bugs spec section 5,
//! `ideaBugSort.ts`): when this computer first claims the plan queued for a person's idea, the idea's text is classified
//! once, and a confident bug is marked (`idea_mark_bug`) so its plan is written as a fix. Anything else, or any error,
//! leaves it an idea; it never blocks the draft.
use std::future::Future;

use crate::ask::classify::{CONFIDENCE, Classified, Kind};
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

/// Pure: the idea moves only on a `bug` at or above [`CONFIDENCE`], and only when the draft is its own plan.
pub fn should_sort(idea: Option<&StoreIdea>, draft_id: &str, result: &Classified) -> bool {
    idea.is_some_and(|i| i.plan_id.as_deref() == Some(draft_id) && !i.bug && i.pictures.is_empty())
        && matches!(result, Classified::Sorted(s) if s.kind == Kind::Bug && s.confidence >= CONFIDENCE)
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

/// Called with a draft just claimed from the store's queue. On a confident bug, marks the idea and rewrites the
/// claimed copy's request as a fix; answers whether it did. `classify` gets the idea's text.
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
    let result = classify(text_of(idea)).await;
    if !should_sort(Some(idea), &draft.id, &result) {
        tracing::info!("ideas: {} stays an idea ({result:?})", idea.id);
        return false;
    }
    if let Err(e) = ideas.mark_bug(&idea.id).await {
        return left(&draft.id, &e.to_string());
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
    use crate::ask::classify::Sorted;

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
    fn only_a_confident_bug_on_its_own_plain_idea_moves() {
        let i = idea("d1");
        assert!(should_sort(Some(&i), "d1", &sorted(Kind::Bug, 0.8)));
        assert!(!should_sort(Some(&i), "d1", &sorted(Kind::Bug, 0.79)));
        assert!(!should_sort(Some(&i), "d1", &sorted(Kind::Idea, 1.0)));
        assert!(!should_sort(Some(&i), "d1", &Classified::Unavailable));
        assert!(!should_sort(Some(&i), "other", &sorted(Kind::Bug, 1.0)));
        assert!(!should_sort(None, "d1", &sorted(Kind::Bug, 1.0)));
        let pic = StoreIdea {
            pictures: vec!["a.png".into()],
            ..idea("d1")
        };
        assert!(!should_sort(Some(&pic), "d1", &sorted(Kind::Bug, 1.0)));
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

    async fn store(marks: std::sync::Arc<std::sync::atomic::AtomicU32>, list: serde_json::Value) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = axum::Router::new().route(
            "/v1/repos/{id}/ops",
            axum::routing::post(move |axum::Json(req): axum::Json<serde_json::Value>| {
                let (marks, list) = (marks.clone(), list.clone());
                async move {
                    if req["op"] == "idea_mark_bug" {
                        marks.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        return axum::Json(json!({}));
                    }
                    axum::Json(json!({ "value": { "round": null, "ideas": list } }))
                }
            }),
        );
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    async fn run(list: serde_json::Value, result: Classified) -> (bool, ClaimedDraft, u32) {
        let marks = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let base = store(marks.clone(), list).await;
        let client = crate::backlog::Client::new(&base);
        let ideas = Ideas {
            client: &client,
            repo_id: 1,
            token: None,
        };
        let mut d = draft();
        let moved = sort(&ideas, &mut d, |_| async move { result }).await;
        (moved, d, marks.load(std::sync::atomic::Ordering::SeqCst))
    }

    fn row(i: &StoreIdea) -> serde_json::Value {
        json!({ "id": i.id, "title": i.title, "state": "waiting", "at": 0, "planId": i.plan_id,
            "addedBy": { "login": "ann", "at": 1 }, "idea": { "experience": "Nothing happens" },
            "pictures": i.pictures })
    }

    #[tokio::test]
    async fn a_confident_bug_is_marked_and_its_request_becomes_a_fix() {
        let (moved, d, marks) = run(json!([row(&idea("d1"))]), sorted(Kind::Bug, 0.9)).await;
        assert!(moved && marks == 1);
        assert_eq!(d.intake.unwrap().kinds, ["fix"]);
        assert!(d.request.unwrap().contains("Plan it as a fix"));
    }

    #[tokio::test]
    async fn an_idea_a_failed_classifier_and_a_picture_bearing_idea_stay_ideas() {
        let plain = row(&idea("d1"));
        for result in [sorted(Kind::Idea, 0.95), Classified::Unavailable] {
            let (moved, d, marks) = run(json!([plain.clone()]), result).await;
            assert!(!moved && marks == 0 && d.request.as_deref() == Some("old"));
        }
        let mut pic = plain.clone();
        pic["pictures"] = json!(["a.png"]);
        let (moved, _, marks) = run(json!([pic]), sorted(Kind::Bug, 1.0)).await;
        assert!(!moved && marks == 0);
    }
}
