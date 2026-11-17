//! The pictures a Continue carries by id (`driver/attached.rs`): fetched from the store, their paths after the notes,
//! and the command answered once they are in.
use super::asks::{PICTURE, store_failing};
use super::*;
use crate::view::{Attached, ItemCommand};

fn attached(ids: &[&str]) -> Option<Attached> {
    Some(Attached {
        command_id: "c1".into(),
        ids: ids.iter().map(|s| s.to_string()).collect(),
    })
}

#[tokio::test]
async fn the_pictures_paths_go_after_the_notes_and_the_command_is_acked() {
    let (base, _asked, _listed) = store_failing("").await;
    let rig = rig(2, &[&base]);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    rig.machine.links.item_commands.set_out("p0", Some(tx));
    let lp = &rig.machine.projects[0];
    let notes = super::super::attached::notes_with_pictures(
        (&lp.client, lp.repo),
        "p0",
        &rig.machine.links,
        Some("See this.".into()),
        attached(&[PICTURE]),
    )
    .await
    .unwrap()
    .unwrap();
    let path = crate::ask::pictures::folder().join(PICTURE);
    assert_eq!(
        notes,
        format!(
            "See this.\n\nAttached images (look at each with your Read tool before acting on this):\n- {}",
            path.display()
        )
    );
    assert!(path.is_file(), "kept for the session that runs later");
    assert_eq!(rx.try_recv().unwrap(), json!({ "type": "ack", "id": "c1" }));
}

#[tokio::test]
async fn a_picture_the_store_wont_give_refuses_the_command() {
    let (base, _asked, _listed) = store_failing("").await;
    let rig = rig(2, &[&base]);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    rig.machine.links.item_commands.set_out("p0", Some(tx));
    let lp = &rig.machine.projects[0];
    let gone = "0a1b2c3d-0000-4000-8000-0000000000ff.png";
    let done = super::super::attached::notes_with_pictures(
        (&lp.client, lp.repo),
        "p0",
        &rig.machine.links,
        Some("See this.".into()),
        attached(&[gone]),
    )
    .await;
    assert_eq!(done, None);
    let answer = rx.try_recv().unwrap();
    assert_eq!(answer["type"], "nack");
    assert_eq!(answer["id"], "c1");
    assert!(
        answer["reason"]
            .as_str()
            .unwrap()
            .starts_with("Couldn't fetch a picture from the shared backlog")
    );
}

#[tokio::test]
async fn a_continue_with_pictures_starts_the_implementer_with_them_after_the_notes() {
    let (base, _asked, _listed) = store_failing("").await;
    let (mut rig, fake) = super::commands::stopped_at(2, Some("s1"), &base);
    rig.machine.links.item_commands.push(ItemCommand::Continue {
        project: "p0".into(),
        item: "a".into(),
        notes: Some("Use the other API.".into()),
        attached: attached(&[PICTURE]),
    });
    rig.machine.poll().await;
    let started = fake.started.lock().unwrap();
    let message = started[0].follow_up.as_deref().unwrap();
    assert!(
        message.starts_with("Use the other API.\n\nAttached images"),
        "{message}"
    );
    assert!(message.contains(PICTURE));
}
