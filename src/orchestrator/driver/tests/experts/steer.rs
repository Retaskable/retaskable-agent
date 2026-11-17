//! A message sent while a turn runs: written to its open input and taken as it works, or left for the next turn.
use super::*;

fn notes(x: &Ex) -> Vec<String> {
    (x.st.args("report").iter())
        .filter_map(|n| n["note"].as_str().map(String::from))
        .collect()
}

fn queued(x: &Ex) -> usize {
    x.expert(ID)
        .and_then(|e| e["queue"].as_array().map(Vec::len))
        .unwrap_or(0)
}

fn steered(x: &Ex) -> Vec<String> {
    x.host.steered.lock().unwrap().clone()
}

/// A turn that takes messages as it works, running.
async fn steering(x: &mut Ex) {
    x.host.steering.store(true, Ordering::SeqCst);
    x.start("s1", "Fix it");
    x.until("the turn running", |x| x.jobs().len() == 1).await;
    // Only this turn takes messages as it works; the next one runs and ends on its own.
    x.host.steering.store(false, Ordering::SeqCst);
}

async fn finish_turn(x: &mut Ex) {
    x.host.go.notify_one();
    x.until("idle", |x| x.detail(ID).as_deref() == Some("expert_idle"))
        .await;
}

#[tokio::test]
async fn a_message_sent_while_a_turn_runs_is_written_to_it_and_is_read_as_it_is_taken() {
    let mut x = ex().await;
    x.host.steer_takes.store(true, Ordering::SeqCst);
    steering(&mut x).await;
    x.say("m", ID, "and this too", false);
    x.until("written to the turn", |x| steered(x) == ["and this too"]).await;
    x.until("taken, so off the queue", |x| {
        queued(x) == 0 && x.st.count("expert_queue_done") == 1
    })
    .await;
    let done = x.st.args("expert_queue_done");
    assert_eq!(done[0]["ids"].as_array().map(Vec::len), Some(1), "its row is read, now");
    finish_turn(&mut x).await;
    x.quiet().await;
    assert_eq!(x.jobs().len(), 1, "no turn of its own after: it was part of this one");
}

#[tokio::test]
async fn a_message_the_session_never_took_runs_as_the_next_turn_still_in_the_queue_meanwhile() {
    let mut x = ex().await;
    steering(&mut x).await;
    x.say("m", ID, "late one", false);
    x.until("written", |x| steered(x) == ["late one"]).await;
    x.quiet().await;
    assert_eq!(queued(&x), 1, "shown until the session says it took it");
    finish_turn(&mut x).await;
    x.until("its own turn", |x| x.jobs().len() == 2).await;
    assert_eq!(x.jobs()[1].message, "late one", "its turn after, as it would have been");
}

#[tokio::test]
async fn a_turn_ended_before_the_last_has_its_answer_in_the_history() {
    let mut x = ex().await;
    x.host.steer_takes.store(true, Ordering::SeqCst);
    *x.host.steer_turn.lock().unwrap() = Some("first part done".into());
    steering(&mut x).await;
    x.say("m", ID, "next", false);
    x.until("its answer noted", |x| {
        notes(x).contains(&"first part done".to_string())
    })
    .await;
    finish_turn(&mut x).await;
    let said = notes(&x);
    let at = |t: &str| {
        said.iter()
            .position(|n| n == t)
            .unwrap_or_else(|| panic!("{t}: {said:?}"))
    };
    assert!(at("first part done") < at("Did: Fix it"), "{said:?}");
}

#[tokio::test]
async fn a_message_with_pictures_or_after_the_input_closed_waits_for_the_next_turn() {
    let mut x = ex().await;
    x.host.steer_takes.store(true, Ordering::SeqCst);
    steering(&mut x).await;
    x.send(
        "pic",
        "ada",
        ExpertAction::Say {
            item: ID.into(),
            text: "look".into(),
            pictures: vec!["0a1b2c3d-0000-4000-8000-00000000000a.png".into()],
            now: false,
        },
    );
    x.quiet().await;
    assert!(
        steered(&x).is_empty() && queued(&x) == 1,
        "pictures are fetched when their turn starts"
    );
    // Another one behind it isn't written ahead of it either: first in, first out.
    x.say("m", ID, "behind", false);
    x.quiet().await;
    assert!(steered(&x).is_empty() && queued(&x) == 2);
    finish_turn(&mut x).await;
    x.until("the two run in order", |x| x.jobs().len() == 3).await;
    assert!(x.jobs()[1].message.starts_with("look") && x.jobs()[2].message == "behind");
}

#[tokio::test]
async fn an_input_that_has_closed_takes_nothing() {
    let mut x = ex().await;
    x.host.steer_closed.store(true, Ordering::SeqCst);
    steering(&mut x).await;
    x.say("m", ID, "too late", false);
    x.quiet().await;
    assert!(steered(&x).is_empty());
    finish_turn(&mut x).await;
    x.until("its own turn", |x| x.jobs().len() == 2).await;
    assert_eq!(x.jobs()[1].message, "too late");
}

#[tokio::test]
async fn send_now_still_cuts_the_turn() {
    let mut x = ex().await;
    steering(&mut x).await;
    x.say("m", ID, "right now", true);
    x.until("the cut turn's successor", |x| x.jobs().len() == 2).await;
    assert!(steered(&x).is_empty(), "Send now isn't steered: the turn is stopped");
    assert!(
        x.jobs()[1].message.starts_with(crate::expert::MID_STEP),
        "{}",
        x.jobs()[1].message
    );
}

#[tokio::test]
async fn what_helpers_found_goes_with_the_message_written_to_the_turn() {
    let mut x = ex().await;
    x.host.steer_takes.store(true, Ordering::SeqCst);
    steering(&mut x).await;
    // A helper that ends while the turn works: its result waits for the person's next message.
    x.send(
        "h",
        "ada",
        ExpertAction::Helper {
            item: ID.into(),
            task: "t".into(),
            lane: false,
        },
    );
    x.answer("h").await;
    x.until("its end noted", |x| {
        notes(x).contains(&"Helper 1 (\"t\") finished: found t".to_string())
    })
    .await;
    x.say("m", ID, "carry on", false);
    x.until("written", |x| steered(x).len() == 1).await;
    assert_eq!(steered(&x)[0], "Helper 1 (\"t\") finished: found t\n\ncarry on");
}
