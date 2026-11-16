//! The rows the hub writes for Add spec and Build this (`INTENT_COMMANDS` in `shared/storeCommands.ts`; the shapes are
//! those of `cloud/test/hubStoreCommands.test.ts`).
use std::sync::Mutex;

use axum::extract::Path as UrlPath;
use axum::routing::{get, post};
use axum::{Json, Router};

use super::*;

const PICTURE: &str = "0a1b2c3d-0000-4000-8000-0000000000a1.png";

type Asked = Arc<Mutex<Vec<(String, Value)>>>;

/// A store with one waiting idea, the starter audiences, one open trial draft when `trial_open`, and one picture.
async fn store(trial_open: bool) -> (WantedCx, Asked) {
    let asked: Asked = Default::default();
    let seen = asked.clone();
    let app = Router::new()
        .route(
            "/v1/repos/{id}/ops",
            post(move |Json(body): Json<Value>| {
                let seen = seen.clone();
                async move {
                    let op = body["op"].as_str().unwrap_or("").to_string();
                    seen.lock().unwrap().push((op.clone(), body["args"].clone()));
                    let value = match op.as_str() {
                        "ideas_list" => json!({ "round": null, "ideas": [
                            { "id": "idea-p1", "title": "Dark mode", "state": "waiting", "at": 1,
                              "idea": { "why": "Eyes tire.", "for": ["dev"] } }] }),
                        "draft_queue" => json!({ "draft": { "id": "draft-new" } }),
                        "drafts" if trial_open => json!([
                            { "id": "draft-done", "status": "merged", "intake": { "trial": true } },
                            { "id": "draft-trial", "status": "drafting", "intake": { "trial": true } }]),
                        "drafts" => json!([{ "id": "draft-other", "status": "queued", "intake": { "chat": true } }]),
                        "facts_get" => json!({ "audiences": { "kind": "audiences", "updatedAt": 1, "by": "a", "data": {
                            "added": [{ "id": "players", "name": "Players", "who": "People who play." }] } } }),
                        _ => Value::Null,
                    };
                    Json(json!({ "value": value }))
                }
            }),
        )
        .route(
            "/v1/repos/{id}/pictures/{picture}",
            get(|UrlPath((_, picture)): UrlPath<(u64, String)>| async move {
                if picture == PICTURE {
                    (axum::http::StatusCode::OK, picture.into_bytes())
                } else {
                    (axum::http::StatusCode::NOT_FOUND, Vec::new())
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let pictures = std::env::temp_dir().join(format!("retaskable-wanted-test-{}", std::process::id()));
    let cx = WantedCx {
        client: Arc::new(Client::new(base)),
        repo: 7,
        token: "tok".into(),
        pictures,
    };
    (cx, asked)
}

fn row(args: Value) -> IntentRow {
    IntentRow {
        id: "in-1".into(),
        kind: "draft_wanted".into(),
        args,
        issued_by: Some("ana".into()),
        status: "claimed".into(),
    }
}

fn ops(asked: &Asked, name: &str) -> Vec<Value> {
    asked
        .lock()
        .unwrap()
        .iter()
        .filter(|(op, _)| op == name)
        .map(|(_, args)| args.clone())
        .collect()
}

#[tokio::test]
async fn add_spec_queues_the_draft_for_the_requester_and_answers_its_id() {
    let (cx, asked) = store(false).await;
    // `draft_spec { request: "Add a dark mode" }`, as the hub writes the row.
    let done = wanted(&cx, &row(json!({ "request": "Add a dark mode" }))).await;
    assert_eq!(done, Outcome::Done(json!({ "id": "draft-new" })));
    let sent = ops(&asked, "draft_queue");
    assert_eq!(
        sent,
        [
            json!({ "request": "Add a dark mode", "titles": ["Add a dark mode"], "requestedBy": "ana",
            "assignee": "ana", "intake": { "chat": true } })
        ]
    );
    // No audience was named, so none is sent (null would say it is for nobody).
    assert!(sent[0].get("audience").is_none());
}

#[tokio::test]
async fn the_title_is_the_first_line_and_a_chore_tag_names_its_chore() {
    let (cx, asked) = store(false).await;
    let request = "[Scan] Fix the headers\n\nAll of them.";
    wanted(&cx, &row(json!({ "request": request }))).await;
    let sent = &ops(&asked, "draft_queue")[0];
    assert_eq!(sent["titles"], json!(["[Scan] Fix the headers"]));
    assert_eq!(sent["chore"], "scan");
    assert_eq!(sent["request"], request);
}

#[tokio::test]
async fn an_issuer_that_is_not_a_login_is_nobody() {
    let (cx, asked) = store(false).await;
    let mut r = row(json!({ "request": "x" }));
    r.issued_by = Some("not a login!".into());
    wanted(&cx, &r).await;
    let sent = &ops(&asked, "draft_queue")[0];
    assert!(sent.get("requestedBy").is_none() && sent.get("assignee").is_none());
}

#[tokio::test]
async fn an_audience_must_be_on_the_companys_list() {
    let (cx, asked) = store(false).await;
    let ok = wanted(&cx, &row(json!({ "request": "A bell", "audience": "players" }))).await;
    assert_eq!(ok, Outcome::Done(json!({ "id": "draft-new" })));
    assert_eq!(ops(&asked, "draft_queue")[0]["audience"], "players");
    let off = wanted(&cx, &row(json!({ "request": "A bell", "audience": "strangers" }))).await;
    assert_eq!(
        off,
        Outcome::Refused("That audience is not on this company's list.".into())
    );
    assert_eq!(ops(&asked, "draft_queue").len(), 1);
    // `null` is for nobody, and is sent as it is.
    wanted(&cx, &row(json!({ "request": "A bell", "audience": null }))).await;
    assert_eq!(ops(&asked, "draft_queue")[1]["audience"], Value::Null);
}

#[tokio::test]
async fn the_trial_change_is_made_once_while_one_is_open() {
    let (cx, asked) = store(false).await;
    let args = json!({ "request": "A first change", "trial": true });
    assert_eq!(
        wanted(&cx, &row(args.clone())).await,
        Outcome::Done(json!({ "id": "draft-new" }))
    );
    assert_eq!(
        ops(&asked, "draft_queue")[0]["intake"],
        json!({ "trial": true, "chat": true })
    );
    let (cx, asked) = store(true).await;
    assert_eq!(
        wanted(&cx, &row(args)).await,
        Outcome::Done(json!({ "id": "draft-trial" }))
    );
    assert!(ops(&asked, "draft_queue").is_empty());
}

#[tokio::test]
async fn pictures_are_fetched_and_listed_after_the_request() {
    let (cx, asked) = store(false).await;
    // `draft_spec { request, pictureIds }` is written as `{ request, pictures }`.
    let done = wanted(&cx, &row(json!({ "request": "A bell", "pictures": [PICTURE] }))).await;
    assert_eq!(done, Outcome::Done(json!({ "id": "draft-new" })));
    let request = ops(&asked, "draft_queue")[0]["request"].as_str().unwrap().to_string();
    assert!(
        request.starts_with("A bell\n\nAttached images (look at each with your Read tool before acting on this):\n- ")
    );
    assert!(request.contains(PICTURE));
    assert!(
        cx.pictures.join(PICTURE).is_file(),
        "kept for the drafter that runs later"
    );
    let gone = "0a1b2c3d-0000-4000-8000-0000000000ff.png";
    let failed = wanted(&cx, &row(json!({ "request": "A bell", "pictures": [gone] }))).await;
    assert!(
        matches!(failed, Outcome::Failed(why) if why.starts_with("Couldn't fetch a picture from the shared backlog"))
    );
    let _ = std::fs::remove_dir_all(&cx.pictures);
}

#[tokio::test]
async fn build_this_accepts_the_idea() {
    let (cx, asked) = store(false).await;
    // `idea_accept { idea: "idea-p1" }`.
    let done = wanted(&cx, &row(json!({ "idea": "idea-p1" }))).await;
    assert_eq!(done, Outcome::Done(Value::Null));
    let names: Vec<String> = asked.lock().unwrap().iter().map(|(op, _)| op.clone()).collect();
    assert_eq!(names, ["ideas_list", "draft_queue", "idea_answer"]);
    let gone = wanted(&cx, &row(json!({ "idea": "nope" }))).await;
    assert_eq!(gone, Outcome::Refused("That idea isn't there any more.".into()));
}

#[tokio::test]
async fn a_row_that_is_not_a_request_is_refused_in_words() {
    let (cx, asked) = store(false).await;
    for (args, why) in [
        (json!({}), "request: Required"),
        (json!({ "request": 4 }), "request: Required"),
        (json!({ "request": "x", "audience": "Not An Id" }), "audience: Invalid"),
        (
            json!({ "request": "x", "trial": false }),
            "trial: Invalid literal value, expected true",
        ),
        (
            json!({ "request": "x", "pictures": ["../x.png"] }),
            "pictureIds: Invalid",
        ),
    ] {
        assert_eq!(wanted(&cx, &row(args)).await, Outcome::Refused(why.into()));
    }
    assert_eq!(
        wanted(&cx, &row(json!({ "request": "  \n " }))).await,
        Outcome::Refused("Describe what the spec should cover.".into())
    );
    assert!(ops(&asked, "draft_queue").is_empty());
}

#[tokio::test]
async fn a_store_that_cannot_be_reached_queues_nothing_and_says_so() {
    let (mut cx, _asked) = store(false).await;
    cx.client = Arc::new(Client::new("http://127.0.0.1:1"));
    let done = wanted(&cx, &row(json!({ "request": "x" }))).await;
    assert!(
        matches!(done, Outcome::Failed(why) if why.starts_with("Can't reach the shared backlog, so no new spec starts until it can be"))
    );
}
