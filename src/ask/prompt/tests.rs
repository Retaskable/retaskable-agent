use super::*;

const TEMPLATE: &str = include_str!("../../../prompts/assistant.md");

fn ask(id: &str, reply_to: Option<&str>) -> Exchange {
    Exchange {
        id: id.into(),
        text: format!("q-{id}"),
        answer: Some(format!("a-{id}")),
        reply_to: reply_to.map(String::from),
        from_retaskable: false,
    }
}

fn ids(list: &[&Exchange]) -> Vec<String> {
    list.iter().map(|a| a.id.clone()).collect()
}

fn item(id: &str) -> StateItem {
    StateItem {
        id: id.into(),
        title: format!("Title {id}"),
        state: "queued".into(),
        backlog_status: "open".into(),
        lane: None,
        change_url: None,
        needs_person: None,
    }
}

fn state(about: Option<&str>) -> StateSummary {
    StateSummary {
        about: about.map(String::from),
        items: vec![item("a-1"), item("b-2"), item("c-3")],
        lanes: vec![StateLane {
            lane: 1,
            item_id: Some("a-1".into()),
            running: true,
        }],
        drafts: vec![StateDraft {
            id: "d1".into(),
            title: "Dark mode".into(),
            state: "drafting".into(),
        }],
        status: "running".into(),
        paused_reason: None,
        spent_today_usd: 1.5,
        daily_usd: 20.0,
    }
}

fn render(asks: &[Exchange], reply_to: Option<&str>, about: Option<&str>, t: ProjectType) -> String {
    let audiences = [AudienceLine {
        id: "players".into(),
        name: "Players".into(),
        who: Some("People who play".into()),
    }];
    build_prompt(
        TEMPLATE,
        &AssistantInput {
            project_type: t,
            base_branch: "main",
            github_repo: "o/r",
            pr: "PR",
            change: "pull request",
            state: &state(about),
            audiences: &audiences,
            asks,
            reply_to,
            question: "why is step 5 stuck?",
        },
    )
    .unwrap()
}

#[test]
fn a_new_ask_gets_the_last_five_exchanges_cleared_ones_included() {
    let asks: Vec<Exchange> = (1..=7).map(|n| ask(&n.to_string(), None)).collect();
    assert_eq!(ids(&select_exchanges(&asks, None)), ["3", "4", "5", "6", "7"]);
    let out = render(&asks, None, None, ProjectType::Web);
    assert!(out.contains("Human: q-3\nYou: a-3") && out.contains("Human: q-7\nYou: a-7"));
    assert!(!out.contains("q-2"));
}

#[test]
fn a_reply_gets_its_thread_and_skips_asks_in_between() {
    let asks = vec![
        ask("1", None),
        ask("2", Some("1")),
        ask("x", None),
        ask("3", Some("2")),
        ask("y", None),
    ];
    assert_eq!(ids(&select_exchanges(&asks, Some("3"))), ["1", "2", "3"]);
    assert_eq!(ids(&select_exchanges(&asks, Some("2"))), ["1", "2"]);
    let out = render(&asks, Some("3"), None, ProjectType::Web);
    assert!(out.contains("q-1") && out.contains("q-3") && !out.contains("q-x") && !out.contains("q-y"));
}

#[test]
fn a_thread_is_cut_to_five_and_a_loop_ends() {
    let mut asks = vec![ask("1", None)];
    for n in 2..=8 {
        asks.push(ask(&n.to_string(), Some(&(n - 1).to_string())));
    }
    assert_eq!(ids(&select_exchanges(&asks, Some("8"))), ["4", "5", "6", "7", "8"]);
    let looped = vec![ask("a", Some("b")), ask("b", Some("a"))];
    assert_eq!(select_exchanges(&looped, Some("a")).len(), 2);
}

#[test]
fn retaskables_messages_show_as_retaskable() {
    let mut r = ask("r", None);
    r.from_retaskable = true;
    r.text = "Do you want to approve it?".into();
    r.answer = None;
    let out = render(&[ask("1", None), r], None, None, ProjectType::Web);
    assert!(out.contains("Retaskable: Do you want to approve it?"));
}

#[test]
fn the_about_item_comes_first_in_the_summary() {
    let out = render(&[], None, Some("c-3"), ProjectType::Web);
    assert!(out.contains("on the page of item c-3"));
    let (c, a) = (out.find("- c-3 ").unwrap(), out.find("- a-1 ").unwrap());
    assert!(c < a && a < out.find("- b-2 ").unwrap());
    assert!(out.contains("Spent today: $1.50 of $20.") && out.contains("- lane 1: item a-1 (running)"));
    assert!(out.contains("- d1 Dark mode: drafting") && out.contains("(none)"));
}

#[test]
fn the_audiences_and_project_words_are_in_the_prompt() {
    let out = render(&[], None, None, ProjectType::Game);
    assert!(out.contains("- `players`: Players. People who play"));
    assert!(out.contains("The project is a game.") && out.contains("launch the game"));
    assert!(out.contains("why is step 5 stuck?") && !out.contains("{{"));
    assert!(render(&[], None, None, ProjectType::Cli).contains("a command-line program or library"));
}

#[test]
fn the_prompt_settles_requests_mixed_messages_and_replies_to_retaskable() {
    let out = render(&[], None, None, ProjectType::Game);
    assert!(out.contains("a spec request, even when it ends in a question mark"));
    assert!(out.contains("A message has at most one `draft_spec`"));
    assert!(out.contains("approving is the **Approve** button under that message"));
    assert!(out.contains("Leaving a field out is not the same as `null`"));
    // The message is fenced off from the instructions, and the advice on answering comes after it.
    let fenced = out.replace("\r\n", "\n");
    let at = fenced.find("<message>\nwhy is step 5 stuck?\n</message>").unwrap();
    assert!(at < fenced.find("## How to answer").unwrap());
}
