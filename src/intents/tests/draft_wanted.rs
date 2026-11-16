//! The `draft_wanted` kind (`intents/draft_wanted.rs`): a row with `idea` is the idea accept, a row without is refused,
//! and a paused project or a computer signed out of Claude leaves it open.
use tokio::sync::mpsc;

use super::*;
use crate::intents::draft_wanted::{AcceptIdea, DraftWantedHandler, NO_DRAFT_SPEC};

fn wanted(id: &str, args: Value) -> IntentRow {
    IntentRow {
        args,
        ..row(id, "draft_wanted", "requested")
    }
}

/// A puller with the handler, and a stand-in for the loop that answers each ask with `answer` and notes it.
fn with_loop(answer: Result<(), String>) -> (Puller<u32>, Arc<Mutex<Vec<String>>>) {
    let (asks, mut waiting) = mpsc::unbounded_channel::<AcceptIdea>();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let noted = seen.clone();
    tokio::spawn(async move {
        while let Some(ask) = waiting.recv().await {
            noted.lock().unwrap().push(format!("{} by {}", ask.idea, ask.by));
            let _ = ask.reply.send(answer.clone());
        }
    });
    let p = Puller::default()
        .register(Arc::new(DraftWantedHandler::new(asks)))
        .timing(Duration::from_secs(60), Duration::from_millis(1));
    (p, seen)
}

fn finishes(fake: &Fake) -> Vec<String> {
    fake.log().into_iter().filter(|l| l.starts_with("finish")).collect()
}

fn claims(fake: &Fake) -> Vec<String> {
    fake.log().into_iter().filter(|l| l.starts_with("claim")).collect()
}

#[tokio::test(start_paused = true)]
async fn a_row_with_an_idea_runs_the_idea_accept_and_ends_done() {
    let fake = Fake::with(vec![wanted("in-1", json!({ "idea": "i1" }))]);
    let (mut p, seen) = with_loop(Ok(()));
    let store: Arc<dyn IntentStore> = fake.clone();
    assert_eq!(p.pass_gated(&store, &Gates::default(), || 0).await, 1);
    settle(&mut p).await;
    assert_eq!(*seen.lock().unwrap(), ["i1 by ana"]);
    assert_eq!(finishes(&fake).len(), 1);
    assert!(finishes(&fake)[0].starts_with("finish in-1 Done"));
}

#[tokio::test(start_paused = true)]
async fn an_idea_the_loop_won_t_queue_ends_refused_with_its_reason() {
    let fake = Fake::with(vec![wanted("in-1", json!({ "idea": "i1" }))]);
    let (mut p, _) = with_loop(Err("That idea isn't there any more.".into()));
    let store: Arc<dyn IntentStore> = fake.clone();
    assert_eq!(p.pass_gated(&store, &Gates::default(), || 0).await, 1);
    settle(&mut p).await;
    assert_eq!(finishes(&fake).len(), 1);
    assert!(finishes(&fake)[0].starts_with("finish in-1 Refused"));
    assert!(finishes(&fake)[0].contains("That idea isn't there any more."));
}

#[tokio::test(start_paused = true)]
async fn a_paused_project_leaves_the_row_open() {
    let fake = Fake::with(vec![wanted("in-1", json!({ "idea": "i1" }))]);
    let (mut p, seen) = with_loop(Ok(()));
    let store: Arc<dyn IntentStore> = fake.clone();
    let paused = Gates {
        paused: true,
        ..Default::default()
    };
    assert_eq!(p.pass_gated(&store, &paused, || 0).await, 0);
    assert!(claims(&fake).is_empty() && finishes(&fake).is_empty());
    assert!(seen.lock().unwrap().is_empty());
    // Resumed: the same row is taken.
    assert_eq!(p.pass_gated(&store, &Gates::default(), || 0).await, 1);
    settle(&mut p).await;
    assert_eq!(claims(&fake), ["claim in-1"]);
}

#[tokio::test(start_paused = true)]
async fn a_computer_signed_out_of_claude_leaves_the_row_open() {
    let fake = Fake::with(vec![wanted("in-1", json!({ "idea": "i1" }))]);
    let (mut p, seen) = with_loop(Ok(()));
    let store: Arc<dyn IntentStore> = fake.clone();
    let out = Gates {
        signed_out: true,
        ..Default::default()
    };
    assert_eq!(p.pass_gated(&store, &out, || 0).await, 0);
    assert!(claims(&fake).is_empty() && finishes(&fake).is_empty());
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_row_without_an_idea_ends_refused_never_failed() {
    let args = json!({ "request": "Add dark mode", "pictures": [] });
    for args in [args, json!({ "idea": " " })] {
        let fake = Fake::with(vec![wanted("in-1", args)]);
        let (mut p, seen) = with_loop(Ok(()));
        let store: Arc<dyn IntentStore> = fake.clone();
        assert_eq!(p.pass_gated(&store, &Gates::default(), || 0).await, 1);
        settle(&mut p).await;
        assert!(seen.lock().unwrap().is_empty());
        let ended = finishes(&fake);
        assert_eq!(ended.len(), 1);
        assert!(ended[0].starts_with("finish in-1 Refused"), "{ended:?}");
        assert!(ended[0].contains(NO_DRAFT_SPEC));
    }
}
