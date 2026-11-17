//! A finish the store hasn't taken survives a restart in the middle of an outage (`keep.rs`): it is in the puller's file
//! before it is first sent, is sent again at once by the next start (and its row is not run again), and goes from the
//! file when the store answers, whatever it says.
use super::*;
use crate::intents::keep::{Keep, Owed};

fn down() -> VecDeque<Result<bool, BacklogError>> {
    std::iter::repeat_with(|| Err(bad())).take(500).collect()
}

fn kept(p: Puller<u32>, dir: &std::path::Path) -> Puller<u32> {
    p.keeping(dir)
}

fn owed(dir: &std::path::Path) -> Vec<String> {
    Keep::open(dir, &["ask"]).owed().into_keys().collect()
}

/// The process dies: nothing in memory is left, and the tasks that were retrying stop with it.
fn die(p: Puller<u32>) {
    for task in &p.tasks {
        task.abort();
    }
}

#[tokio::test(start_paused = true)]
async fn a_restart_in_an_outage_sends_the_owed_finish_at_once_and_it_is_taken_once_the_store_is_back() {
    let dir = tempfile::tempdir().unwrap();
    let before = Fake::with(vec![row("in-1", "ask", "requested")]);
    *before.finishes.lock().unwrap() = down();
    let (h, runs) = Fixed::new("ask", Outcome::Done(json!({ "answer": "In a.rs." })));
    let mut p = kept(puller(h), dir.path());
    let store: Arc<dyn IntentStore> = before.clone();
    p.pass(&store, || 0).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(finishes(&before), 3, "still trying");
    assert_eq!(owed(dir.path()), ["in-1"], "on disk, listed before the first send");
    die(p);

    // The computer starts again with the store still out; the row is listed as this computer's own, running.
    let after = Fake::with(vec![row("in-1", "ask", "running")]);
    *after.finishes.lock().unwrap() = VecDeque::from([Err(bad()), Err(bad())]);
    let (h, runs_after) = Fixed::new("ask", Outcome::Done(json!({})));
    let mut p = kept(puller(h), dir.path());
    let store: Arc<dyn IntentStore> = after.clone();
    assert_eq!(p.pass(&store, || 0).await, 0, "nothing new is started");
    tokio::time::sleep(Duration::from_millis(1)).await;
    assert_eq!(finishes(&after), 1, "sent at once");
    assert_eq!(
        after.log().iter().find(|l| l.starts_with("finish")).unwrap(),
        r#"finish in-1 Done {"answer":"In a.rs."} null"#,
        "with what the first run answered"
    );
    assert!(runs_after.lock().unwrap().is_empty(), "the row is not run again");
    assert_eq!(runs.lock().unwrap().len(), 1);
    assert_eq!(owed(dir.path()), ["in-1"], "still owed");

    // The store is back: taken once, then off the disk.
    tokio::time::sleep(FINISH_SLOW * 3).await;
    assert_eq!(finishes(&after), 3, "two refused, one taken, no more");
    settle(&mut p).await;
    assert!(owed(dir.path()).is_empty());
    assert_eq!(p.pass(&store, || 0).await, 0);
    assert_eq!(finishes(&after), 3);
}

#[tokio::test(start_paused = true)]
async fn an_owed_finish_the_store_says_moved_on_is_dropped_and_not_sent_again() {
    let dir = tempfile::tempdir().unwrap();
    let before = Fake::with(vec![row("in-1", "ask", "requested")]);
    *before.finishes.lock().unwrap() = down();
    let (h, _) = Fixed::new("ask", Outcome::Failed("Broke.".into()));
    let mut p = kept(puller(h), dir.path());
    let store: Arc<dyn IntentStore> = before.clone();
    p.pass(&store, || 0).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    die(p);

    let after = Fake::with(vec![row("in-1", "ask", "claimed")]);
    *after.finishes.lock().unwrap() = VecDeque::from([Ok(false)]);
    let (h, _) = Fixed::new("ask", Outcome::Done(json!({})));
    let mut p = kept(puller(h), dir.path());
    let store: Arc<dyn IntentStore> = after.clone();
    p.pass(&store, || 0).await;
    settle(&mut p).await;
    assert_eq!(finishes(&after), 1);
    assert!(owed(dir.path()).is_empty(), "dropped");
    tokio::time::sleep(FINISH_SLOW * 3).await;
    assert_eq!(finishes(&after), 1, "not sent again");
}

#[tokio::test(start_paused = true)]
async fn a_row_still_running_when_the_process_dies_is_not_kept() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Fake::with(vec![row("in-1", "ask", "requested")]);
    let (h, _) = Fixed::new("ask", Outcome::Abandoned);
    let mut p = kept(puller(h), dir.path());
    let store: Arc<dyn IntentStore> = fake.clone();
    p.pass(&store, || 0).await;
    settle(&mut p).await;
    assert!(owed(dir.path()).is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_finish_owed_for_a_day_is_given_up_on_after_one_more_try() {
    let dir = tempfile::tempdir().unwrap();
    let keep = Keep::open(dir.path(), &["ask"]);
    keep.add("in-1", Ending::Done, &Some(json!({})), &None);
    // Listed two days ago.
    let file = dir.path().join("intent_finish.ask.json");
    let mut text: std::collections::BTreeMap<String, Owed> =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    text.get_mut("in-1").unwrap().since -= 2 * 24 * 60 * 60;
    std::fs::write(&file, serde_json::to_string(&text).unwrap()).unwrap();

    let fake = Fake::with(vec![row("in-1", "ask", "running")]);
    *fake.finishes.lock().unwrap() = down();
    let (h, _) = Fixed::new("ask", Outcome::Done(json!({})));
    let mut p = kept(puller(h), dir.path());
    let store: Arc<dyn IntentStore> = fake.clone();
    p.pass(&store, || 0).await;
    settle(&mut p).await;
    assert_eq!(finishes(&fake), 1, "one try, then it is let go");
    assert!(owed(dir.path()).is_empty());
}
