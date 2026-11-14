use serde_json::json;

use super::*;
use crate::backlog::take_over::Owner;

#[derive(Default)]
struct Fake {
    answer: Mutex<Option<Result<PersonClaim, String>>>,
    calls: Mutex<Vec<String>>,
}

impl Fake {
    fn answering(answer: Result<PersonClaim, String>) -> Arc<Fake> {
        Arc::new(Fake {
            answer: Mutex::new(Some(answer)),
            ..Default::default()
        })
    }
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl Taker for Fake {
    fn take(&self, id: &str) -> BoxFuture<'static, Result<PersonClaim, String>> {
        self.calls.lock().unwrap().push(format!("take {id}"));
        let answer = self.answer.lock().unwrap().take().expect("one take");
        async move { answer }.boxed()
    }
    fn give_back(&self, id: &str, token: &str) -> BoxFuture<'static, ()> {
        self.calls.lock().unwrap().push(format!("give back {id} {token}"));
        async {}.boxed()
    }
}

fn claim(waiting: Option<&str>) -> PersonClaim {
    PersonClaim {
        item: Item {
            id: "4".into(),
            title: "T".into(),
            ..Default::default()
        },
        token: "t1".into(),
        was_waiting: waiting.map(String::from),
        was_owner: Some(Owner {
            login: Some("ana".into()),
            machine: Some("ana-pc".into()),
        }),
    }
}

fn row(args: serde_json::Value) -> IntentRow {
    IntentRow {
        id: "r1".into(),
        kind: "take_over_requested".into(),
        args,
        issued_by: Some("ana".into()),
        status: "claimed".into(),
    }
}

fn handler() -> (TakeOverHandler, Arc<TakeOvers>) {
    let shared = Arc::new(TakeOvers::default());
    (TakeOverHandler::new(shared.clone()), shared)
}

fn cx(fake: &Arc<Fake>) -> Arc<dyn Taker> {
    fake.clone()
}

#[test]
fn only_a_row_naming_an_item_is_taken() {
    let (h, _) = handler();
    assert!(h.takes(&row(json!({ "itemId": "4" }))));
    assert!(!h.takes(&row(json!({}))));
    assert!(!h.takes(&row(json!({ "itemId": "" }))));
    assert!(!h.takes(&row(json!({ "itemId": 4 }))));
}

#[tokio::test]
async fn an_item_waiting_for_help_is_handed_to_the_loop() {
    let fake = Fake::answering(Ok(claim(Some("help"))));
    let (h, shared) = handler();
    let out = h.run(row(json!({ "itemId": "4" })), &cx(&fake)).await;
    assert_eq!(out, Outcome::Done(json!({ "itemId": "4" })));
    assert_eq!(fake.calls(), vec!["take 4"]);
    let taken = shared.drain();
    assert_eq!(taken.len(), 1);
    assert_eq!((taken[0].item.id.as_str(), taken[0].token.as_str()), ("4", "t1"));
    assert!(shared.drain().is_empty(), "each item once");
}

#[tokio::test]
async fn an_item_not_waiting_for_help_is_given_back_and_refused() {
    for waiting in [None, Some("ci"), Some("answer"), Some("tryout"), Some("merge")] {
        let fake = Fake::answering(Ok(claim(waiting)));
        let (h, shared) = handler();
        let out = h.run(row(json!({ "itemId": "4" })), &cx(&fake)).await;
        assert_eq!(
            out,
            Outcome::Refused("It isn't waiting for help on another computer, so there's nothing to take over.".into()),
            "{waiting:?}"
        );
        assert_eq!(fake.calls(), vec!["take 4", "give back 4 t1"], "{waiting:?}");
        assert!(shared.drain().is_empty());
    }
}

#[tokio::test]
async fn the_stores_refusal_is_the_rows_refusal() {
    let fake = Fake::answering(Err("ana-pc has been online in the last 2 hours".into()));
    let (h, shared) = handler();
    let out = h.run(row(json!({ "itemId": "4" })), &cx(&fake)).await;
    assert_eq!(
        out,
        Outcome::Refused("ana-pc has been online in the last 2 hours".into())
    );
    assert!(shared.drain().is_empty());
}

#[tokio::test]
async fn an_item_already_held_here_is_refused_without_a_claim() {
    let fake = Fake::answering(Err("never asked".into()));
    let (h, shared) = handler();
    shared.set_held(["4".to_string()]);
    let out = h.run(row(json!({ "itemId": "4" })), &cx(&fake)).await;
    assert_eq!(out, Outcome::Refused("It's on this computer already.".into()));
    assert!(fake.calls().is_empty());
}

#[tokio::test]
async fn an_item_just_taken_counts_as_held_until_the_loop_has_it() {
    let fake = Fake::answering(Ok(claim(Some("help"))));
    let (h, shared) = handler();
    h.run(row(json!({ "itemId": "4" })), &cx(&fake)).await;
    // The loop reports what it holds before it has adopted the item: the item taken still counts.
    shared.set_held([]);
    let again = Fake::answering(Err("never asked".into()));
    let out = h.run(row(json!({ "itemId": "4" })), &cx(&again)).await;
    assert_eq!(out, Outcome::Refused("It's on this computer already.".into()));
    assert!(again.calls().is_empty());
}
