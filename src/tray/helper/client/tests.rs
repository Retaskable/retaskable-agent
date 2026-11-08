use super::*;
use tokio::io::{AsyncReadExt, DuplexStream, duplex};

fn item(id: &str, text: &str, action: Option<Action>) -> Item {
    Item {
        id: id.into(),
        text: text.into(),
        enabled: true,
        hidden: false,
        checked: false,
        inline: false,
        children: vec![],
        action,
    }
}

fn frame(items: Vec<Item>, icon: &str, tooltip: &str) -> Frame {
    Frame {
        menu: Menu { items },
        icon: icon.into(),
        tooltip: tooltip.into(),
    }
}

fn base() -> Vec<Item> {
    let mut parent = item("link", "Link to", None);
    parent.children = vec![item("link.child", "Production", Some(Action::Pause))];
    let mut inline = item("req", "Approve", None);
    inline.inline = true;
    inline.children = vec![item("req.do", "Open the page", Some(Action::Settings))];
    vec![item("quit", "Quit", Some(Action::QuitNow)), parent, inline]
}

/// The helper's end: reads what the agent sends, writes what the helper would.
struct Fake {
    to_agent: DuplexStream,
    from_agent: BufReader<DuplexStream>,
}

impl Fake {
    async fn say(&mut self, line: &str) {
        self.to_agent.write_all(format!("{line}\n").as_bytes()).await.unwrap();
    }
    async fn line(&mut self) -> Value {
        let mut s = String::new();
        self.from_agent.read_line(&mut s).await.unwrap();
        serde_json::from_str(&s).unwrap()
    }
}

async fn connected(frame: &Frame) -> (TrayHelper, Fake) {
    let (agent_r, to_agent) = duplex(1 << 16);
    let (from_agent, agent_w) = duplex(1 << 16);
    let mut fake = Fake {
        to_agent,
        from_agent: BufReader::new(from_agent),
    };
    fake.say(r#"{"type":"ready"}"#).await;
    let h = TrayHelper::connect(agent_r, agent_w, frame, Duration::from_secs(5))
        .await
        .unwrap();
    (h, fake)
}

#[tokio::test]
async fn nothing_is_sent_before_ready_and_the_first_line_is_the_whole_menu() {
    let f = frame(base(), "ICON", "tip");
    let (agent_r, mut to_agent) = duplex(1 << 16);
    let (from_agent, agent_w) = duplex(1 << 16);
    let mut from_agent = BufReader::new(from_agent);
    let task = tokio::spawn(async move { TrayHelper::connect(agent_r, agent_w, &f, Duration::from_secs(5)).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let mut buf = [0u8; 1];
    assert!(
        tokio::time::timeout(Duration::from_millis(50), from_agent.read(&mut buf))
            .await
            .is_err()
    );
    to_agent.write_all(b"{\"type\":\"ready\"}\n").await.unwrap();
    let _h = task.await.unwrap().unwrap();
    let mut s = String::new();
    from_agent.read_line(&mut s).await.unwrap();
    let v: Value = serde_json::from_str(&s).unwrap();
    assert_eq!(v["icon"], "ICON");
    assert_eq!(v["tooltip"], "tip");
    let items = v["items"].as_array().unwrap();
    // quit, link (with child), req, then req's line drawn under it.
    assert_eq!(items.len(), 4);
    assert_eq!(
        items[0],
        json!({"title":"Quit","tooltip":"","enabled":true,"checked":false,"hidden":false,"items":[],"__id":1})
    );
    assert_eq!(items[1]["items"][0]["title"], "Production");
    assert_eq!(items[1]["items"][0]["__id"], 3);
    assert_eq!(items[3]["title"], "      Open the page");
}

#[tokio::test]
async fn a_later_change_sends_only_what_changed_parents_first() {
    let (mut h, mut fake) = connected(&frame(base(), "I", "t")).await;
    let _ = fake.line().await;
    h.update(&frame(base(), "I", "t")).await.unwrap();
    let mut changed = base();
    changed[1].text = "Link to…".into();
    changed[1].children[0].enabled = false;
    h.update(&frame(changed, "I2", "t")).await.unwrap();
    let a = fake.line().await;
    assert_eq!(
        (a["type"].as_str(), a["item"]["title"].as_str(), a["seq_id"].as_i64()),
        (Some("update-item"), Some("Link to…"), Some(-1))
    );
    assert_eq!(a["item"]["__id"], 2);
    let b = fake.line().await;
    assert_eq!(
        (b["item"]["__id"].as_u64(), b["item"]["enabled"].as_bool()),
        (Some(3), Some(false))
    );
    let c = fake.line().await;
    assert_eq!(c["type"], "update-menu");
    assert_eq!(c["menu"]["icon"], "I2");
    // Nothing more: the next thing sent is the exit.
    h.exit().await;
    assert_eq!(fake.line().await, json!({"type": "exit"}));
}

#[tokio::test]
async fn an_inline_item_is_hidden_with_its_lines() {
    let (mut h, mut fake) = connected(&frame(base(), "I", "t")).await;
    let _ = fake.line().await;
    let mut hidden = base();
    hidden[2].hidden = true;
    h.update(&frame(hidden, "I", "t")).await.unwrap();
    let parent = fake.line().await;
    let child = fake.line().await;
    assert_eq!(
        (parent["item"]["__id"].as_u64(), parent["item"]["hidden"].as_bool()),
        (Some(4), Some(true))
    );
    assert_eq!(
        (child["item"]["__id"].as_u64(), child["item"]["hidden"].as_bool()),
        (Some(5), Some(true))
    );
}

#[tokio::test]
async fn a_click_yields_the_action_of_a_shown_item_only() {
    let mut items = base();
    items[0].enabled = false; // seq 1
    items[2].hidden = true; // seq 4, its line 5
    let (mut h, mut fake) = connected(&frame(items, "I", "t")).await;
    let _ = fake.line().await;
    for id in [1, 4, 5, 2, 99] {
        fake.say(&format!(r#"{{"type":"clicked","__id":{id}}}"#)).await;
    }
    fake.say("not json").await;
    fake.say(r#"{"type":"clicked","__id":3}"#).await;
    assert_eq!(h.next().await, TrayEvent::Chose(Action::Pause));
    // After a change, the item as last shown decides.
    let mut now = base();
    now[0].enabled = true;
    h.draw(&frame(now, "I", "t")).await.unwrap();
    fake.say(r#"{"type":"clicked","__id":1}"#).await;
    assert_eq!(h.next().await, TrayEvent::Chose(Action::QuitNow));
}

#[tokio::test]
async fn a_helper_not_ready_in_time_or_exiting_is_reported() {
    let (agent_r, _keep) = duplex(64);
    let (_r, agent_w) = duplex(64);
    let err = TrayHelper::connect(agent_r, agent_w, &frame(base(), "I", "t"), Duration::from_millis(50))
        .await
        .err();
    assert!(err.unwrap().contains("didn't start within"));

    let (agent_r, gone) = duplex(64);
    let (_r, agent_w) = duplex(64);
    drop(gone);
    let err = TrayHelper::connect(agent_r, agent_w, &frame(base(), "I", "t"), Duration::from_secs(5))
        .await
        .err();
    assert!(err.unwrap().contains("stopped before it was ready"));

    let (mut h, fake) = connected(&frame(base(), "I", "t")).await;
    drop(fake);
    assert!(matches!(h.next().await, TrayEvent::Stopped(_)));
    assert!(matches!(h.next().await, TrayEvent::Stopped(_)));
}

#[tokio::test]
async fn exit_sends_the_message_and_closes_stdin() {
    let (mut h, mut fake) = connected(&frame(base(), "I", "t")).await;
    let _ = fake.line().await;
    h.stop().await;
    assert_eq!(fake.line().await, json!({"type": "exit"}));
    let mut rest = String::new();
    fake.from_agent.read_to_string(&mut rest).await.unwrap();
    assert_eq!(rest, "");
}
