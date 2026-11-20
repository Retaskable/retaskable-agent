//! Sorting what the plus box sent (docs/specs/idea-sort.md, `ideaBugSort.ts`; it replaces the bug classifier of
//! questions-and-triage.md): when this computer first claims the plan queued for a person's idea, the idea's words are sorted
//! once ([`super::sort_idea`]: the cheap check, then one read-only session) and the sort is written with `idea_sort`. The store
//! moves a `bug` to Bugs and keeps `visible` for the design review's gate; the claimed copy's request is rewritten as a fix to
//! match. A computer that can't answer writes `later`, and [`sort_later`] sorts those when a pass has nothing to claim. Never
//! blocks the draft, and never changes the person's own words.
use std::collections::HashMap;
use std::future::Future;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::backlog::drafts::ClaimedDraft;
use crate::design_board::DesignError;

use super::accept::person_request;
use super::sort_idea::{IdeaSort, SortKind, pictured, sort_text};
use super::store::{IdeaState, Ideas, StoreIdea};

/// The idea a draft is the plan of, when it is a waiting person's idea no one has sorted (or only as `later`) and that is not
/// already a bug.
pub fn sortable<'a>(ideas: &'a [StoreIdea], draft_id: &str) -> Option<&'a StoreIdea> {
    ideas.iter().find(|i| {
        i.state == IdeaState::Waiting
            && i.added_by.is_some()
            && i.plan_id.as_deref() == Some(draft_id)
            && !i.bug
            && i.sort.as_ref().is_none_or(|s| s.kind == "later")
    })
}

/// A first claim: not a hand-off, a move, an answer or a send-back.
pub fn first_claim(draft: &ClaimedDraft) -> bool {
    let set = |k: &str| draft.other.get(k).is_some_and(|v| !v.is_null() && v != false);
    draft.intake.is_some() && draft.answer.is_none() && !["movedFrom", "handedOff", "revise"].iter().any(|k| set(k))
}

/// The text a sort reads: the title and, when it adds to it, what they'd see.
fn text_of(idea: &StoreIdea) -> String {
    let experience = idea.idea.as_ref().map_or("", |b| b.experience.as_str());
    if experience.is_empty() || experience == idea.title {
        idea.title.trim().to_string()
    } else {
        format!("{}\n\n{experience}", idea.title).trim().to_string()
    }
}

/// Called with a draft just claimed from the store's queue. Sorts the idea's text and writes the sort; when it is a bug the store
/// accepted, rewrites the claimed copy's request as a fix and answers true. `ask` runs the read-only session (its prompt in, its
/// structured answer out).
pub async fn sort<F, Fut>(ideas: &Ideas<'_>, draft: &mut ClaimedDraft, ask: F) -> bool
where
    F: FnOnce(String) -> Fut,
    Fut: Future<Output = Result<Value, DesignError>>,
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
    let sorted = if idea.pictures.is_empty() {
        sort_text(&text_of(idea), ask).await
    } else {
        pictured(&idea.title)
    };
    if let Err(e) = ideas.sort(&idea.id, &sorted).await {
        return left(&draft.id, &e.to_string());
    }
    tracing::info!(
        "ideas: {} sorted: {}{}",
        idea.id,
        sorted.kind.name(),
        if sorted.by_rule { " (by rule)" } else { "" }
    );
    if sorted.kind != SortKind::Bug {
        return false;
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
    true
}

fn left(draft: &str, why: &str) -> bool {
    tracing::info!("ideas: {draft} left unsorted: {why}");
    false
}

/// How often a project's ideas are read for ones marked "Sorting later", and how often one idea is tried again.
pub const RESORT_AFTER: Duration = Duration::from_secs(10 * 60);
/// At most this many are sorted in a pass.
const LATER_AT_A_TIME: usize = 3;

/// When each project's ideas were last read for `later` ones and each such idea last tried, so a computer that still can't
/// answer is not asked again at once. Held in memory only.
#[derive(Default)]
pub struct LaterTries {
    listed: HashMap<u64, Instant>,
    tried: HashMap<(u64, String), Instant>,
}

static TRIES: Mutex<Option<LaterTries>> = Mutex::new(None);

impl LaterTries {
    /// Whether the project's ideas may be read now; when they may, it is noted.
    fn listing_due(&mut self, repo: u64, now: Instant) -> bool {
        if self
            .listed
            .get(&repo)
            .is_some_and(|at| now.duration_since(*at) < RESORT_AFTER)
        {
            return false;
        }
        self.listed.insert(repo, now);
        true
    }

    /// The waiting `later` ideas (no bug mark, no picture) not tried in the last ten minutes, at most three, noted as tried.
    fn due<'a>(&mut self, repo: u64, ideas: &'a [StoreIdea], now: Instant) -> Vec<&'a StoreIdea> {
        let mut out = Vec::new();
        for i in ideas {
            let later = i.sort.as_ref().is_some_and(|s| s.kind == "later");
            let key = (repo, i.id.clone());
            let fresh = self
                .tried
                .get(&key)
                .is_none_or(|at| now.duration_since(*at) >= RESORT_AFTER);
            if i.state == IdeaState::Waiting
                && !i.bug
                && i.pictures.is_empty()
                && later
                && fresh
                && out.len() < LATER_AT_A_TIME
            {
                self.tried.insert(key, now);
                out.push(i);
            }
        }
        out
    }
}

/// The computer is back and has nothing to claim: each waiting idea marked "Sorting later" is sorted now, a few at a time, and
/// only a real answer is written. Reads the project's ideas at most every ten minutes; answers how many were written.
pub async fn sort_later<F, Fut>(ideas: &Ideas<'_>, mut ask: F) -> usize
where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = Result<Value, DesignError>>,
{
    let now = Instant::now();
    let listing = TRIES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(Default::default)
        .listing_due(ideas.repo_id, now);
    if !listing {
        return 0;
    }
    let Ok(list) = ideas.list().await else { return 0 };
    let due: Vec<&StoreIdea> = TRIES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(Default::default)
        .due(ideas.repo_id, &list.ideas, now);
    write_later(ideas, &due, &mut ask).await
}

async fn write_later<F, Fut>(ideas: &Ideas<'_>, due: &[&StoreIdea], ask: &mut F) -> usize
where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = Result<Value, DesignError>>,
{
    let mut done = 0;
    for idea in due {
        let sorted: IdeaSort = sort_text(&text_of(idea), &mut *ask).await;
        if sorted.kind == SortKind::Later {
            continue;
        }
        match ideas.sort(&idea.id, &sorted).await {
            Ok(_) => done += 1,
            Err(e) => tracing::info!("ideas: {} still not sorted: {e}", idea.id),
        }
    }
    done
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex as StdMutex};

    use serde_json::json;

    use super::*;

    type Sent = Arc<StdMutex<Vec<Value>>>;

    fn idea(plan: &str) -> StoreIdea {
        serde_json::from_value(row(plan, json!({}))).unwrap()
    }

    fn row(plan: &str, extra: Value) -> Value {
        let mut r = json!({
            "id": "i1", "title": "Save button dead", "state": "waiting", "at": 0,
            "idea": { "experience": "Looks off" },
            "addedBy": { "login": "ann", "at": 1 }, "planId": plan
        });
        r.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        r
    }

    fn draft() -> ClaimedDraft {
        serde_json::from_value(json!({ "id": "d1", "request": "old", "intake": { "kinds": ["feature"] } })).unwrap()
    }

    #[test]
    fn only_a_waiting_persons_unsorted_idea_is_sortable_and_a_later_one_still_is() {
        let i = idea("d1");
        assert!(sortable(std::slice::from_ref(&i), "d1").is_some());
        assert!(sortable(std::slice::from_ref(&i), "other").is_none());
        let marked: StoreIdea = serde_json::from_value(row("d1", json!({ "bug": true }))).unwrap();
        assert!(sortable(&[marked], "d1").is_none());
        let sorted: StoreIdea =
            serde_json::from_value(row("d1", json!({ "sort": { "kind": "idea", "by": "person" } }))).unwrap();
        assert!(sortable(&[sorted], "d1").is_none(), "a sorted idea is left");
        let later: StoreIdea = serde_json::from_value(row("d1", json!({ "sort": { "kind": "later" } }))).unwrap();
        assert!(sortable(&[later], "d1").is_some());
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

    /// A store answering `ideas_list` with `list` and `idea_sort` as the real one does: every sort is kept, and `conflict` refuses one
    /// when `refuse` is set (a person already sorted it).
    async fn store(sent: Sent, list: Value, refuse: bool) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = axum::Router::new().route(
            "/v1/repos/{id}/ops",
            axum::routing::post(move |axum::Json(req): axum::Json<Value>| {
                let (sent, list) = (sent.clone(), list.clone());
                async move {
                    if req["op"] == "idea_sort" {
                        sent.lock().unwrap().push(req["args"].clone());
                        if refuse {
                            return (
                                axum::http::StatusCode::CONFLICT,
                                axum::Json(json!({ "error": "conflict" })),
                            );
                        }
                        return (
                            axum::http::StatusCode::OK,
                            axum::Json(json!({ "value": { "id": "i1" } })),
                        );
                    }
                    (
                        axum::http::StatusCode::OK,
                        axum::Json(json!({ "value": { "round": null, "ideas": list } })),
                    )
                }
            }),
        );
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    fn answer(kind: &str) -> Result<Value, DesignError> {
        Ok(json!({ "kind": kind, "reason": "fits the Flags screen", "title": "Save is dead", "visible": true }))
    }

    async fn run_with(list: Value, ask: Result<Value, DesignError>, refuse: bool) -> (bool, ClaimedDraft, Vec<Value>) {
        let sent: Sent = Default::default();
        let base = store(sent.clone(), list, refuse).await;
        let client = crate::backlog::Client::new(&base);
        let ideas = Ideas {
            client: &client,
            repo_id: 1,
            token: None,
        };
        let mut d = draft();
        let moved = sort(&ideas, &mut d, |_| async move { ask }).await;
        let sent = sent.lock().unwrap().clone();
        (moved, d, sent)
    }

    #[tokio::test]
    async fn a_bug_the_session_finds_is_sent_with_visible_and_becomes_a_fix() {
        // "Save button dead" has no broken or wish word the rule knows, so the session answers.
        let list = json!([row("d1", json!({ "title": "The save thing" }))]);
        let (moved, d, sent) = run_with(list, answer("bug"), false).await;
        assert_eq!(
            sent,
            [
                json!({ "id": "i1", "kind": "bug", "by": "agent", "reason": "fits the Flags screen", "title": "Save is dead", "visible": true })
            ]
        );
        assert!(moved);
        assert_eq!(d.intake.unwrap().kinds, ["fix"]);
        assert!(d.request.unwrap().contains("Plan it as a fix"));
    }

    #[tokio::test]
    async fn the_rule_sorts_the_obvious_with_no_visible_and_no_session() {
        let list = json!([row(
            "d1",
            json!({ "title": "The save button doesn't work", "idea": { "experience": "" } })
        )]);
        let (moved, _, sent) = run_with(list, Err(DesignError::conflict("not asked")), false).await;
        assert!(moved);
        assert_eq!(sent[0]["by"], "rule");
        assert_eq!(sent[0]["kind"], "bug");
        assert!(sent[0].get("visible").is_none());
        let list = json!([row(
            "d1",
            json!({ "title": "Add a dark mode", "idea": { "experience": "" } })
        )]);
        let (moved, d, sent) = run_with(list, Err(DesignError::conflict("not asked")), false).await;
        assert!(!moved && d.request.as_deref() == Some("old"));
        assert_eq!(sent[0]["kind"], "idea");
    }

    #[tokio::test]
    async fn an_idea_an_unsure_one_a_picture_and_a_failed_session_are_sent_as_such_with_no_rewrite() {
        let plain = row("d1", json!({ "title": "The save thing" }));
        let (moved, d, sent) = run_with(json!([plain.clone()]), answer("unsure"), false).await;
        assert!(!moved && d.request.as_deref() == Some("old"));
        assert_eq!(sent[0]["kind"], "unsure");
        let (moved, _, sent) = run_with(json!([plain.clone()]), Err(DesignError::conflict("signed out")), false).await;
        assert!(!moved);
        assert_eq!(sent, [json!({ "id": "i1", "kind": "later", "by": "rule" })]);
        let mut pic = plain;
        pic["pictures"] = json!(["a.png"]);
        let (moved, _, sent) = run_with(json!([pic]), answer("bug"), false).await;
        assert!(!moved, "a picture is never settled by its words");
        assert_eq!(
            (sent[0]["kind"].as_str(), sent[0]["by"].as_str()),
            (Some("unsure"), Some("rule"))
        );
    }

    #[tokio::test]
    async fn a_sort_the_store_refuses_leaves_the_plan_as_it_was() {
        let list = json!([row("d1", json!({ "title": "The save thing" }))]);
        let (moved, d, sent) = run_with(list, answer("bug"), true).await;
        assert!(!moved && sent.len() == 1);
        assert_eq!(d.request.as_deref(), Some("old"));
    }

    #[test]
    fn later_ideas_are_due_three_at_a_time_and_not_again_for_ten_minutes() {
        let later = |id: &str, extra: Value| -> StoreIdea {
            let mut r = row("p", json!({ "id": id, "sort": { "kind": "later" } }));
            r.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            serde_json::from_value(r).unwrap()
        };
        let list = vec![
            later("a", json!({})),
            later("b", json!({})),
            later("pic", json!({ "pictures": ["x"] })),
            later("bug", json!({ "bug": true })),
            later("c", json!({})),
            later("d", json!({})),
            idea("p"),
        ];
        let mut t = LaterTries::default();
        let now = Instant::now();
        let ids = |v: Vec<&StoreIdea>| v.iter().map(|i| i.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(t.due(1, &list, now)), ["a", "b", "c"]);
        assert_eq!(ids(t.due(1, &list, now + Duration::from_secs(60))), ["d"]);
        assert_eq!(
            ids(t.due(1, &list, now + RESORT_AFTER + Duration::from_secs(1))),
            ["a", "b", "c"]
        );
        assert!(t.listing_due(1, now));
        assert!(!t.listing_due(1, now + Duration::from_secs(5)));
        assert!(t.listing_due(2, now), "another project");
        assert!(t.listing_due(1, now + RESORT_AFTER));
    }

    #[tokio::test]
    async fn sorting_later_writes_only_a_real_answer() {
        let list = json!([
            row(
                "p",
                json!({ "id": "a", "title": "The save thing", "sort": { "kind": "later" } })
            ),
            row(
                "p",
                json!({ "id": "b", "title": "Add a dark mode", "idea": { "experience": "" }, "sort": { "kind": "later" } })
            ),
        ]);
        let sent: Sent = Default::default();
        let base = store(sent.clone(), list, false).await;
        let client = crate::backlog::Client::new(&base);
        let ideas = Ideas {
            client: &client,
            repo_id: 77,
            token: None,
        };
        // The session fails: "a" stays later and is not written; "b" is settled by rule.
        let done = sort_later(&ideas, |_| async { Err(DesignError::conflict("offline")) }).await;
        assert_eq!(done, 1);
        let sent = sent.lock().unwrap().clone();
        assert_eq!(sent.len(), 1);
        assert_eq!(
            (sent[0]["id"].as_str(), sent[0]["kind"].as_str()),
            (Some("b"), Some("idea"))
        );
        // Read again at once: nothing, the project was just read.
        assert_eq!(sort_later(&ideas, |_| async { answer("bug") }).await, 0);
    }
}
