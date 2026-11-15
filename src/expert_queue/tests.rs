use super::*;
use crate::backlog::expert::{RowClaim, RowState};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// An in-process store: the rows and notes the real ops keep, with a switch for the store being out of reach.
#[derive(Default)]
struct World {
    rows: Mutex<BTreeMap<String, Vec<ExpertRow>>>,
    notes: Mutex<BTreeMap<String, ExpertFrom>>,
    down: AtomicBool,
    next: Mutex<u32>,
}

#[derive(Clone)]
struct Fake(Arc<World>);

fn unreachable() -> BacklogError {
    BacklogError::Unreachable("down".into())
}

fn conflict() -> BacklogError {
    BacklogError::BadAnswer {
        status: 409,
        code: Some("conflict".into()),
        message: "taken".into(),
    }
}

impl Fake {
    fn check(&self) -> Result<(), BacklogError> {
        if self.0.down.load(Ordering::SeqCst) {
            Err(unreachable())
        } else {
            Ok(())
        }
    }
    fn rows(&self, item: &str) -> Vec<ExpertRow> {
        self.0.rows.lock().unwrap().get(item).cloned().unwrap_or_default()
    }
}

impl QueueOps for Fake {
    async fn add(&self, item: &str, text: &str, by: &str, pictures: &[String]) -> Result<String, BacklogError> {
        self.check()?;
        let mut n = self.0.next.lock().unwrap();
        *n += 1;
        let id = format!("q-{n}");
        self.0
            .rows
            .lock()
            .unwrap()
            .entry(item.into())
            .or_default()
            .push(ExpertRow {
                id: id.clone(),
                text: text.into(),
                picture_ids: pictures.to_vec(),
                at: 1,
                by: by.into(),
                state: RowState::Queued,
                claim: None,
            });
        Ok(id)
    }
    async fn take(&self, item: &str, ids: &[String], machine: &str) -> Result<Vec<ExpertRow>, BacklogError> {
        self.check()?;
        let mut all = self.0.rows.lock().unwrap();
        let rows = all.entry(item.into()).or_default();
        let want = |r: &ExpertRow| ids.contains(&r.id);
        if rows
            .iter()
            .any(|r| want(r) && r.state == RowState::Taken && r.claim.as_ref().is_some_and(|c| c.machine != machine))
        {
            return Err(conflict());
        }
        let mut got = Vec::new();
        for r in rows.iter_mut().filter(|r| want(r)) {
            r.state = RowState::Taken;
            r.claim = Some(RowClaim {
                agent_id: machine.into(),
                machine: machine.into(),
                at: 2,
            });
            got.push(r.clone());
        }
        Ok(got)
    }
    async fn done(&self, item: &str, ids: &[String]) -> Result<bool, BacklogError> {
        self.check()?;
        let mut all = self.0.rows.lock().unwrap();
        let rows = all.entry(item.into()).or_default();
        let before = rows.len();
        rows.retain(|r| !(ids.contains(&r.id) && r.state == RowState::Taken));
        Ok(before - rows.len() == ids.len())
    }
    async fn clear(&self, item: &str) -> Result<(), BacklogError> {
        self.check()?;
        self.0.rows.lock().unwrap().remove(item);
        self.0.notes.lock().unwrap().remove(item);
        Ok(())
    }
    async fn continues_set(&self, item: &str, from: &ExpertFrom) -> Result<(), BacklogError> {
        self.check()?;
        self.0.notes.lock().unwrap().insert(item.into(), from.clone());
        Ok(())
    }
    async fn list(&self) -> Result<QueueList, BacklogError> {
        self.check()?;
        Ok(QueueList {
            sessions: self.0.rows.lock().unwrap().clone(),
            continues: self.0.notes.lock().unwrap().clone(),
        })
    }
}

fn world() -> (Fake, Arc<World>) {
    let w = Arc::new(World::default());
    (Fake(w.clone()), w)
}

fn from() -> ExpertFrom {
    ExpertFrom {
        session_id: "s-1".into(),
        title: "Chart".into(),
        branch: None,
    }
}

#[tokio::test]
async fn three_queued_messages_are_taken_and_read_in_order() {
    let (fake, _) = world();
    let q = ExpertQueue::new(fake.clone(), "box");
    let mut added = Vec::new();
    for t in ["one", "two", "three"] {
        added.push(q.add("e-1", t, "ada", &[], true).await.unwrap());
    }
    let pulled = q.pull().await.unwrap();
    let texts: Vec<_> = pulled["e-1"].iter().map(|m| m.text.as_str()).collect();
    assert_eq!(texts, ["one", "two", "three"]);
    for m in &added {
        let got = q.take("e-1", std::slice::from_ref(m)).await.unwrap();
        assert_eq!(got[0].text, m.text);
        assert!(q.holding());
        q.done("e-1", std::slice::from_ref(&m.id)).await;
    }
    assert!(!q.holding());
    assert!(fake.rows("e-1").is_empty());
}

#[tokio::test]
async fn two_computers_produce_one_run() {
    let (fake_a, w) = world();
    let a = ExpertQueue::new(fake_a.clone(), "box-a");
    let b = ExpertQueue::new(Fake(w), "box-b");
    let m = a.add("e-1", "hi", "ada", &[], true).await.unwrap();
    assert!(a.take("e-1", std::slice::from_ref(&m)).await.is_some());
    assert!(b.take("e-1", std::slice::from_ref(&m)).await.is_none());
    assert!(!b.holding());
}

#[tokio::test]
async fn a_take_returns_the_text_the_store_has_now() {
    let (fake, w) = world();
    let q = ExpertQueue::new(fake, "box");
    let m = q.add("e-1", "old", "ada", &[], true).await.unwrap();
    w.rows.lock().unwrap().get_mut("e-1").unwrap()[0].text = "edited".into();
    assert_eq!(q.take("e-1", &[m]).await.unwrap()[0].text, "edited");
}

#[tokio::test]
async fn a_cancelled_message_is_not_taken() {
    let (fake, w) = world();
    let q = ExpertQueue::new(fake, "box");
    let m = q.add("e-1", "gone", "ada", &[], true).await.unwrap();
    w.rows.lock().unwrap().clear();
    assert!(q.take("e-1", &[m]).await.is_none());
}

#[tokio::test]
async fn with_the_store_down_a_message_is_local_and_carries_on_from_memory() {
    let (fake, w) = world();
    w.down.store(true, Ordering::SeqCst);
    let q = ExpertQueue::new(fake, "box");
    assert!(q.add("e-1", "x", "ada", &[], true).await.is_err());
    let m = q.add("e-1", "x", "ada", &[], false).await.unwrap();
    assert!(is_local(&m.id));
    let got = q.take("e-1", std::slice::from_ref(&m)).await.unwrap();
    assert_eq!(got, vec![m.clone()]);
    q.done("e-1", std::slice::from_ref(&m.id)).await;
    assert!(!q.holding());
    assert!(q.state().owed.is_empty());
}

#[tokio::test]
async fn a_done_the_store_missed_is_sent_when_it_is_back_and_hidden_meanwhile() {
    let (fake, w) = world();
    let q = ExpertQueue::new(fake.clone(), "box");
    let m = q.add("e-1", "read me", "ada", &[], true).await.unwrap();
    q.take("e-1", std::slice::from_ref(&m)).await.unwrap();
    w.down.store(true, Ordering::SeqCst);
    q.done("e-1", std::slice::from_ref(&m.id)).await;
    assert_eq!(fake.rows("e-1").len(), 1);
    w.down.store(false, Ordering::SeqCst);
    // The row is still in the store, but this computer has consumed it: pull flushes the done first.
    let pulled = q.pull().await.unwrap();
    assert!(pulled.get("e-1").is_none_or(|r| r.is_empty()));
    assert!(fake.rows("e-1").is_empty());
}

#[tokio::test]
async fn a_row_taken_while_the_store_was_down_is_claimed_before_it_is_done() {
    let (fake, w) = world();
    let q = ExpertQueue::new(fake.clone(), "box");
    let m = q.add("e-1", "x", "ada", &[], true).await.unwrap();
    w.down.store(true, Ordering::SeqCst);
    assert!(q.take("e-1", std::slice::from_ref(&m)).await.is_some());
    assert!(!q.holding());
    q.done("e-1", std::slice::from_ref(&m.id)).await;
    w.down.store(false, Ordering::SeqCst);
    q.pull().await.unwrap();
    assert!(fake.rows("e-1").is_empty());
}

#[tokio::test]
async fn the_beat_takes_again_and_lets_go_of_a_row_another_computer_holds() {
    let (fake, w) = world();
    let q = ExpertQueue::new(fake.clone(), "box-a");
    let m = q.add("e-1", "x", "ada", &[], true).await.unwrap();
    q.take("e-1", std::slice::from_ref(&m)).await.unwrap();
    q.beat().await;
    assert!(q.holding());
    w.rows.lock().unwrap().get_mut("e-1").unwrap()[0]
        .claim
        .as_mut()
        .unwrap()
        .machine = "box-b".into();
    q.beat().await;
    assert!(!q.holding());
}

#[tokio::test]
async fn discard_clears_now_or_when_the_store_is_back_and_forgets_the_note() {
    let (fake, w) = world();
    let q = ExpertQueue::new(fake.clone(), "box");
    q.add("e-1", "x", "ada", &[], true).await.unwrap();
    assert!(q.set_continued("e-1", from()).await);
    w.down.store(true, Ordering::SeqCst);
    q.clear("e-1").await;
    assert!(q.continued_from("e-1").is_none());
    assert_eq!(fake.rows("e-1").len(), 1);
    w.down.store(false, Ordering::SeqCst);
    let pulled = q.pull().await.unwrap();
    assert!(!pulled.contains_key("e-1"));
    assert!(fake.rows("e-1").is_empty());
    assert!(q.continued_from("e-1").is_none());
}

#[tokio::test]
async fn a_continuation_note_not_sent_is_sent_by_the_next_pull() {
    let (fake, w) = world();
    let q = ExpertQueue::new(fake, "box");
    w.down.store(true, Ordering::SeqCst);
    assert!(!q.set_continued("e-1", from()).await);
    assert_eq!(q.continued_from("e-1"), Some(from()));
    assert!(q.pull().await.is_none());
    w.down.store(false, Ordering::SeqCst);
    q.pull().await.unwrap();
    assert_eq!(w.notes.lock().unwrap().get("e-1"), Some(&from()));
}

#[tokio::test]
async fn pull_reads_the_notes_another_computer_set_and_the_text_is_capped() {
    let (fake, w) = world();
    w.notes.lock().unwrap().insert("e-2".into(), from());
    let q = ExpertQueue::new(fake.clone(), "box");
    q.pull().await.unwrap();
    assert_eq!(q.continued_from("e-2"), Some(from()));
    let long = "x".repeat(5000);
    let m = q.add("e-2", &long, "ada", &[], true).await.unwrap();
    assert_eq!(m.text.chars().count(), 4000);
    assert_eq!(fake.rows("e-2")[0].text.chars().count(), 4000);
}

#[tokio::test]
async fn pictures_travel_as_ids() {
    let (fake, _) = world();
    let q = ExpertQueue::new(fake.clone(), "box");
    let m = q.add("e-1", "look", "ada", &["p1".into()], true).await.unwrap();
    assert_eq!(m.picture_ids, vec!["p1".to_string()]);
    assert_eq!(fake.rows("e-1")[0].picture_ids, vec!["p1".to_string()]);
}
