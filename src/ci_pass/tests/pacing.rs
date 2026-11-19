//! The pass spreads its calls: a head the store had nothing for is left alone a while, at most [`SENDS_MAX`] heads are
//! asked about in a pass, and a `429` ends the pass with its `Retry-After` reaching the loop.
use super::merge::{list_and_green, pull_by};
use super::*;
use crate::backlog::BacklogError;

const MIN: u64 = 60_000;

fn many(n: u64) -> Vec<Value> {
    (1..=n)
        .map(|i| pull_by(i, &format!("feat/{i}"), "ann", "MERGEABLE"))
        .collect()
}

fn asked(rig: &Rig) -> Vec<u64> {
    rig.store
        .sent("ci_result")
        .iter()
        .map(|f| f["pr"].as_u64().unwrap())
        .collect()
}

#[tokio::test]
async fn a_head_the_store_had_nothing_for_is_not_asked_again_at_once_but_after_the_wait() {
    let rig = Rig::new().await;
    list_and_green(&rig, many(2));
    let mut since = HashMap::new();
    rig.pass(&mut since, 0).await.unwrap();
    rig.pass(&mut since, 2 * MIN).await.unwrap();
    assert_eq!(asked(&rig), [1, 2], "the second pass asked nothing");
    rig.pass(&mut since, 11 * MIN).await.unwrap();
    assert_eq!(asked(&rig), [1, 2, 1, 2]);
}

#[tokio::test]
async fn a_head_unchanged_for_days_is_asked_about_every_six_hours() {
    let rig = Rig::new().await;
    list_and_green(&rig, many(1));
    let mut since = HashMap::new();
    let day = 24 * 60 * MIN;
    rig.pass(&mut since, 0).await.unwrap();
    rig.pass(&mut since, 3 * day).await.unwrap();
    assert_eq!(asked(&rig).len(), 2, "unchanged 3 days: asked again once");
    rig.pass(&mut since, 3 * day + 5 * 60 * MIN).await.unwrap();
    assert_eq!(asked(&rig).len(), 2, "but not within 6 hours of that");
    rig.pass(&mut since, 3 * day + 6 * 60 * MIN).await.unwrap();
    assert_eq!(asked(&rig).len(), 3);
}

#[tokio::test]
async fn a_head_the_store_gave_work_is_asked_again_next_pass() {
    let rig = Rig::new().await;
    list_and_green(&rig, many(1));
    rig.store.answers(
        |_| json!({ "ok": true, "condition": "red", "next": { "do": "rerun", "rerun": "force", "hung": false } }),
    );
    let mut since = HashMap::new();
    rig.pass(&mut since, 0).await.unwrap();
    let before = asked(&rig).len();
    rig.pass(&mut since, MIN).await.unwrap();
    assert!(asked(&rig).len() > before);
}

#[tokio::test]
async fn at_most_so_many_heads_are_asked_in_one_pass_and_the_rest_next_pass() {
    let rig = Rig::new().await;
    let n = SENDS_MAX as u64 + 5;
    list_and_green(&rig, many(n));
    let mut since = HashMap::new();
    rig.pass(&mut since, 0).await.unwrap();
    assert_eq!(asked(&rig), (1..=SENDS_MAX as u64).collect::<Vec<_>>());
    rig.pass(&mut since, 2 * MIN).await.unwrap();
    // The first twelve are quiet, so the rest are asked now.
    assert_eq!(asked(&rig).len(), n as usize);
    assert_eq!(*asked(&rig).last().unwrap(), n);
}

#[tokio::test]
async fn a_429_ends_the_pass_with_its_retry_after_and_asks_no_more() {
    let rig = Rig::new().await;
    list_and_green(&rig, many(5));
    rig.store.too_many(120);
    let err = rig.pass(&mut HashMap::new(), 0).await.unwrap_err();
    assert_eq!(asked(&rig).len(), 1, "the rest of the burst is not sent");
    let e = err
        .downcast_ref::<BacklogError>()
        .expect("the store's error reaches the loop");
    assert_eq!(e.retry_after(), Some(std::time::Duration::from_secs(120)));
}
