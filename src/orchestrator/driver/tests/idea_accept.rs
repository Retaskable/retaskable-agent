//! The `idea_accept` step (`driver/idea_accept.rs`): the loop records the flag, queues the draft in the store and
//! sends the command's answer up the connection.
use super::*;
use crate::view::IdeaCommand;

async fn store() -> (String, Asked) {
    let asked: Asked = Default::default();
    let seen = asked.clone();
    let app = Router::new().route(
        "/v1/repos/{id}/ops",
        post(move |Json(body): Json<Value>| {
            let seen = seen.clone();
            async move {
                let op = body["op"].as_str().unwrap_or("").to_string();
                seen.lock().unwrap().push((op.clone(), body["args"].clone()));
                let value = match op.as_str() {
                    "ideas_list" => json!({ "round": null, "ideas": [
                        { "id": "i1", "title": "Dark mode", "state": "waiting", "at": 1,
                          "idea": { "why": "Eyes tire.", "for": ["dev"] } }] }),
                    "draft_queue" => json!({ "id": "d1" }),
                    "list" => json!([]),
                    _ => Value::Null,
                };
                Json(json!({ "value": value }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), asked)
}

fn press(id: &str, idea: &str) -> IdeaCommand {
    IdeaCommand {
        project: "p0".into(),
        command_id: id.into(),
        idea: idea.into(),
        by: "jc".into(),
    }
}

#[tokio::test]
async fn a_queued_accept_is_written_up_and_acked_and_the_flag_is_recorded_each_pass() {
    let (base, asked) = store().await;
    let mut rig = rig(2, &[&base]);
    rig.go_live();
    let item_commands = rig.machine.links.item_commands.clone();
    rig.machine.pass().await;
    assert!(!item_commands.flag_on("p0", "ideas-session"));
    rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(&["ideas-session"]));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    item_commands.set_out("p0", Some(tx));
    item_commands.push_idea(press("c1", "i1"));
    item_commands.push_idea(press("c2", "gone"));
    rig.machine.pass().await;
    assert!(item_commands.flag_on("p0", "ideas-session"));
    let (first, second) = (rx.try_recv().unwrap(), rx.try_recv().unwrap());
    assert_eq!(first, json!({ "type": "ack", "id": "c1" }));
    assert_eq!(
        second,
        json!({ "type": "nack", "id": "c2", "reason": "That idea isn't there any more." })
    );
    let ops: Vec<String> = asked.lock().unwrap().iter().map(|(op, _)| op.clone()).collect();
    assert!(
        ops.windows(3)
            .any(|w| w == ["ideas_list", "draft_queue", "idea_answer"]),
        "{ops:?}"
    );
}
