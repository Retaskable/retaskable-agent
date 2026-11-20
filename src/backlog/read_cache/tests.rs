use std::time::Duration;

use serde_json::json;

use crate::backlog::Client;
use crate::backlog::tests::store;

fn five_seconds(client: &mut Client) {
    client.reads.fresh = Some(Duration::from_secs(5));
}

#[tokio::test]
async fn a_second_read_of_the_same_thing_in_a_pass_is_not_a_call() {
    let (base, seen) = store(200, json!({ "value": [] })).await;
    let mut client = Client::new(base);
    five_seconds(&mut client);
    client.list(1, Some("t")).await.unwrap();
    client.list(1, Some("t")).await.unwrap();
    client.drafts(1, Some("t")).await.unwrap();
    client.drafts(1, Some("t")).await.unwrap();
    client.list(2, Some("t")).await.unwrap();
    let ops: Vec<_> = seen.lock().unwrap().iter().map(|r| r["op"].clone()).collect();
    assert_eq!(
        ops,
        [json!("list"), json!("drafts"), json!("list")],
        "once each, per project"
    );
}

#[tokio::test]
async fn a_write_to_the_project_drops_what_was_kept_and_an_error_is_not_kept() {
    let (base, seen) = store(200, json!({ "value": [] })).await;
    let mut client = Client::new(base);
    five_seconds(&mut client);
    client.list(1, Some("t")).await.unwrap();
    client.skip(1, Some("t"), "item").await.ok();
    client.list(1, Some("t")).await.unwrap();
    assert_eq!(seen.lock().unwrap().len(), 3, "list, the write, list again");
    let (base, seen) = store(500, json!({ "error": "boom" })).await;
    let mut client = Client::new(base);
    five_seconds(&mut client);
    assert!(client.list(1, Some("t")).await.is_err());
    assert!(client.list(1, Some("t")).await.is_err());
    assert_eq!(seen.lock().unwrap().len(), 2, "a failed read is asked again");
}

#[tokio::test]
async fn reads_asked_at_once_are_one_call() {
    let (base, seen) = store(200, json!({ "value": [] })).await;
    let mut client = Client::new(base);
    five_seconds(&mut client);
    let (a, b, c) = tokio::join!(
        client.list(1, Some("t")),
        client.list(1, Some("t")),
        client.list(1, Some("t"))
    );
    assert!(a.is_ok() && b.is_ok() && c.is_ok());
    assert_eq!(seen.lock().unwrap().len(), 1);
}
