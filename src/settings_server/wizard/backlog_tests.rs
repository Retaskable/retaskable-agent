//! What a repo's backlog already holds (docs/specs/setup-wizard.md section 3.9): the counting by state and the route.
use std::sync::Arc;

use axum::body::Body;
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::backlog_counts::*;
use crate::backlog::{Item, ItemStatus};
use crate::env::Env;
use crate::settings_server::test_support::*;
use crate::settings_server::{SettingsServer, test_keychain};

fn items(states: &[ItemStatus]) -> Vec<Item> {
    states
        .iter()
        .enumerate()
        .map(|(n, s)| Item {
            id: format!("i{n}"),
            status: *s,
            ..Item::default()
        })
        .collect()
}

#[test]
fn counts_go_by_state_and_leave_finished_items_out() {
    use ItemStatus::*;
    let c = count(&items(&[
        Held,
        Todo,
        Todo,
        Claimed,
        Building,
        InReview,
        Merging,
        NeedsPerson,
        OnBranch,
        OnMain,
        InProduction,
        Skipped,
    ]));
    assert_eq!((c.waiting, c.in_progress, c.needs_you, c.total()), (3, 4, 1, 8));
    assert_eq!(count(&[]), Counts::default());
}

#[test]
fn the_answer_names_each_count() {
    let c = Counts {
        in_progress: 3,
        waiting: 36,
        needs_you: 0,
    };
    assert_eq!(
        c.to_json("jamez667/memosy.ai"),
        json!({ "repo": "jamez667/memosy.ai", "inProgress": 3, "waiting": 36, "needsYou": 0, "total": 39 })
    );
}

struct Fixed(Result<Counts, String>);

impl CountSource for Fixed {
    fn counts(&self, _repo: String) -> BoxFuture<'static, Result<Counts, String>> {
        let out = self.0.clone();
        async move { out }.boxed()
    }
}

async fn ask(source: Option<Result<Counts, String>>, path: &str, signed_in: bool) -> (u16, Value) {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let settings = Arc::new(SettingsServer::new(
        data.clone(),
        4700,
        4700,
        false,
        Env::from_pairs([("GH_TOKEN", "x")]),
        || 1_700_000_000,
        |_: i32| {},
        test_keychain(&data),
    ));
    if let Some(s) = source {
        settings.set_wizard_counts(Arc::new(Fixed(s)));
    }
    let app = app(settings.clone());
    let (cookie, _) = sign_in(&app, &settings).await;
    let mut req = request("GET", path);
    if signed_in {
        req = req.header("Cookie", &cookie);
    }
    let res = app.oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
    (res.status().as_u16(), json_body(res).await)
}

#[tokio::test]
async fn the_route_answers_the_counts_and_the_words_when_it_cannot() {
    let c = Counts {
        in_progress: 3,
        waiting: 36,
        needs_you: 0,
    };
    let (status, v) = ask(Some(Ok(c)), "/api/wizard/backlog?repo=o/r", true).await;
    assert_eq!(
        (status, v["total"].as_u64(), v["inProgress"].as_u64()),
        (200, Some(39), Some(3))
    );
    let (_, v) = ask(
        Some(Err("Couldn't reach retaskable.com.".into())),
        "/api/wizard/backlog?repo=o/r",
        true,
    )
    .await;
    assert_eq!(v["error"], "Couldn't reach retaskable.com.");
    assert!(v.get("total").is_none(), "an unknown is never a zero");
}

#[tokio::test]
async fn the_route_wants_a_session_and_a_repo_name() {
    assert_eq!(
        ask(Some(Ok(Counts::default())), "/api/wizard/backlog?repo=o/r", false)
            .await
            .0,
        401
    );
    assert_eq!(
        ask(Some(Ok(Counts::default())), "/api/wizard/backlog?repo=nope", true)
            .await
            .0,
        400
    );
}
