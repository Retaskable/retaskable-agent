//! A result the store hasn't taken survives a restart in the middle of an outage: it is in the project's file before it
//! is first sent, is sent again at once by the next start (its request, still `running` in the store, is not run or
//! claimed again), and goes from the file when the store answers, whatever it says.
use super::*;

fn owed(dir: &Path) -> Vec<String> {
    OwedFile::<Owed>::open(dir.join(FINISH_FILE))
        .all()
        .into_keys()
        .collect()
}

/// The first run of "dr-1" ends `done` with `{ "pushed": 3 }` while the store is out for good (the old process's own
/// retries go on against this fake and never get through, as the process is gone in the test's mind).
async fn finished_in_an_outage(dir: &Path) {
    let before = Fake::with(vec![request("dr-1", DeployKind::Hosting)]);
    before.state(|s| {
        s.finish_down = usize::MAX;
        s.runs = VecDeque::from([Ok(done(map(json!({ "pushed": 3 }))))]);
    });
    let mut loop_ = DeployRequests::new(before.clone()).keeping(dir);
    loop_.cycle().await;
    settle().await;
    assert_eq!(owed(dir), ["dr-1"], "on disk before it is sent");
}

fn running_in_the_store() -> Fake {
    let mut row = request("dr-1", DeployKind::Hosting);
    row.status = DeployStatus::Running;
    Fake::with(vec![row])
}

#[tokio::test(start_paused = true)]
async fn a_restart_in_an_outage_sends_the_owed_result_at_once_and_it_is_taken_once_the_store_is_back() {
    let dir = tempfile::tempdir().unwrap();
    finished_in_an_outage(dir.path()).await;

    let after = running_in_the_store();
    after.state(|s| s.finish_down = 2);
    let mut loop_ = DeployRequests::new(after.clone()).keeping(dir.path());
    loop_.cycle().await;
    settle().await;
    after.state(|s| {
        assert_eq!(s.finish_tries, 1, "sent at once");
        assert!(s.ran.is_empty() && s.claims.is_empty(), "never run or claimed again");
    });
    assert_eq!(loop_.running(), 1, "held until the store answers");
    assert_eq!(owed(dir.path()), ["dr-1"]);

    tokio::time::sleep(WAIT * 3).await;
    after.state(|s| {
        assert_eq!(s.finish_tries, 3, "two refused, one taken, no more");
        assert_eq!(
            s.finished,
            [("dr-1".to_string(), Finish::Done, Some(map(json!({ "pushed": 3 }))))],
            "with what the first run answered"
        );
    });
    assert_eq!(loop_.running(), 0);
    assert!(owed(dir.path()).is_empty());
    loop_.cycle().await;
    tokio::time::sleep(WAIT * 3).await;
    after.state(|s| assert_eq!(s.finish_tries, 3, "taken once"));
}

#[tokio::test(start_paused = true)]
async fn an_owed_result_the_store_says_moved_on_is_dropped_and_not_sent_again() {
    let dir = tempfile::tempdir().unwrap();
    finished_in_an_outage(dir.path()).await;
    let after = running_in_the_store();
    after.state(|s| s.finish_moved_on = true);
    let mut loop_ = DeployRequests::new(after.clone()).keeping(dir.path());
    loop_.cycle().await;
    tokio::time::sleep(WAIT * 3).await;
    after.state(|s| assert_eq!(s.finish_tries, 1));
    assert!(owed(dir.path()).is_empty());
    assert_eq!(loop_.running(), 0);
}

#[tokio::test(start_paused = true)]
async fn a_result_owed_for_a_day_is_given_up_on_after_one_more_try() {
    let dir = tempfile::tempdir().unwrap();
    finished_in_an_outage(dir.path()).await;
    let file = OwedFile::<Owed>::open(dir.path().join(FINISH_FILE));
    let mut entry = file.get("dr-1").unwrap();
    entry.since -= 2 * 24 * 60 * 60;
    file.put("dr-1", entry);

    let after = running_in_the_store();
    after.state(|s| s.finish_down = usize::MAX);
    let mut loop_ = DeployRequests::new(after.clone()).keeping(dir.path());
    loop_.cycle().await;
    settle().await;
    after.state(|s| assert_eq!(s.finish_tries, 1));
    assert!(owed(dir.path()).is_empty());
    assert_eq!(loop_.running(), 0);
}
