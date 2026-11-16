//! A message's pictures (the store's ids in its queue row): fetched for the turn, listed after the message, gone
//! when the turn ends, and never a path in the store.
use super::*;

const A: &str = "0a1b2c3d-0000-4000-8000-00000000000a.png";
const B: &str = "0a1b2c3d-0000-4000-8000-00000000000b.webp";
const GONE: &str = "0a1b2c3d-0000-4000-8000-0000000000ff.png";

fn say_with(x: &Ex, id: &str, text: &str, ids: &[&str]) {
    x.send(
        id,
        "ada",
        ExpertAction::Say {
            item: ID.into(),
            text: text.into(),
            pictures: ids.iter().map(ToString::to_string).collect(),
            now: false,
        },
    );
}

#[tokio::test]
async fn a_message_with_pictures_runs_with_them_listed_and_removes_them_after() {
    let mut x = ex().await;
    started(&mut x).await;
    say_with(&x, "m1", "look at these", &[A, B]);
    assert_eq!(x.answer("m1").await["type"], "ack");
    x.until("the second turn", |x| {
        x.jobs().len() == 2 && x.detail(ID).as_deref() == Some("expert_idle")
    })
    .await;
    let j = &x.jobs()[1];
    assert!(
        j.message
            .starts_with("look at these\n\nAttached images (look at each with your Read tool")
    );
    assert!(j.message.contains(A) && j.message.contains(B), "{}", j.message);
    assert!(j.pictures_there);
    // The row carried the ids, not paths; the history line is the answer alone.
    let add = &x.st.args("expert_queue_add")[1];
    assert_eq!(add["pictureIds"], json!([A, B]));
    assert!(!add["text"].as_str().unwrap().contains("Attached images"));
    // The turn is over: the pictures and their folder are gone.
    let path = std::path::PathBuf::from(j.message.lines().last().unwrap().trim_start_matches("- "));
    assert!(!path.exists(), "{}", path.display());
    assert!(!path.parent().unwrap().exists());
}

#[tokio::test]
async fn a_picture_the_store_cant_give_drops_the_message_and_says_so() {
    let mut x = ex().await;
    started(&mut x).await;
    say_with(&x, "m1", "this one", &[A, GONE]);
    assert_eq!(x.answer("m1").await["type"], "ack");
    x.until("the message read", |x| x.st.count("expert_queue_done") == 2)
        .await;
    assert_eq!(x.jobs().len(), 1, "no turn ran");
    let notes = x.st.args("report");
    assert!(
        notes.iter().any(|n| n["note"]
            .as_str()
            .is_some_and(|t| t.starts_with("Couldn't read the attached pictures.") && t.contains("send it again"))),
        "{notes:?}"
    );
    // The next message runs as usual.
    x.say("m2", ID, "never mind", false);
    x.until("the next turn", |x| x.jobs().len() == 2).await;
    assert_eq!(x.jobs()[1].message, "never mind");
}

#[tokio::test]
async fn a_message_with_no_pictures_has_no_block() {
    let mut x = ex().await;
    started(&mut x).await;
    x.say("m1", ID, "plain", false);
    x.until("the turn", |x| x.jobs().len() == 2).await;
    assert_eq!(x.jobs()[1].message, "plain");
}
