use super::*;
use serde_json::json;

fn meta(at: i64) -> String {
    json!({ "type": "retaskable_meta", "role": "implementer", "engine": "claude", "at": at }).to_string()
}
fn said(text: &str) -> String {
    json!({ "type": "assistant", "message": { "content": [{ "type": "text", "text": text }] } }).to_string()
}
fn tool(name: &str, input: Value) -> String {
    json!({ "type": "assistant", "message": { "content": [{ "type": "tool_use", "id": "x", "name": name, "input": input }] } })
        .to_string()
}

fn turn(at: i64, n: usize) -> Turn {
    Turn {
        at,
        events: (0..n)
            .map(|i| SessionEvent {
                at,
                kind: EventKind::Text,
                text: format!("{at}-{i}"),
            })
            .collect(),
        said: None,
    }
}

fn texts(t: &Turn) -> Vec<&str> {
    t.events.iter().map(|e| e.text.as_str()).collect()
}

#[test]
fn the_runs_file_is_read_back_as_turns_each_meta_line_starting_one() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("s.jsonl");
    let lines = [
        meta(100),
        said("Looking."),
        tool("Read", json!({ "file_path": "a.ts" })),
        "not json".into(),
        meta(200),
        tool("Edit", json!({ "file_path": "a.ts" })),
        String::new(),
    ];
    std::fs::write(&file, lines.join("\n")).unwrap();
    let turns = read_turns(&file);
    let shown: Vec<(i64, Vec<&str>)> = turns.iter().map(|t| (t.at, texts(t))).collect();
    assert_eq!(
        shown,
        [(100, vec!["Looking.", "Read: a.ts"]), (200, vec!["Edit: a.ts"])]
    );
    assert!(read_turns(&dir.path().join("missing.jsonl")).is_empty());
}

#[test]
fn newest_turns_first_up_to_the_limit_the_running_one_left_to_watch() {
    let turns = [turn(100, 3), turn(200, 2), turn(300, 4), turn(400, 1)];
    let (all, more) = pick_turns(
        &turns,
        Pick {
            running: true,
            ..Pick::default()
        },
    );
    assert_eq!(
        all.iter().map(|t| t.at).collect::<Vec<_>>(),
        [100, 200, 300],
        "the running turn is watch's"
    );
    assert!(!more);

    let limit = |l, before| Pick {
        limit: Some(l),
        before,
        running: false,
    };
    let (some, more) = pick_turns(&turns, limit(5, None));
    assert_eq!(
        some.iter().map(|t| (t.at, t.events.len())).collect::<Vec<_>>(),
        [(300, 4), (400, 1)]
    );
    assert!(more);

    let (cut, more) = pick_turns(&turns, limit(5, Some(400)));
    assert_eq!(
        cut.iter().map(|t| (t.at, texts(t))).collect::<Vec<_>>(),
        [(200, vec!["200-1"]), (300, vec!["300-0", "300-1", "300-2", "300-3"])],
        "the oldest kept turn is cut to fit"
    );
    assert!(more);

    let (none, more) = pick_turns(
        &turns,
        Pick {
            before: Some(100),
            ..Pick::default()
        },
    );
    assert!(none.is_empty() && !more);
}

#[test]
fn a_continued_session_starts_with_its_claude_code_conversation() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("s1.jsonl");
    let at = |s: u32| format!("2026-10-30T00:00:{s:02}.000Z");
    let person = |text: &str, s: u32, more: Value| {
        let mut v = json!({ "type": "user", "timestamp": at(s), "message": { "content": text } });
        v.as_object_mut().unwrap().extend(more.as_object().unwrap().clone());
        v.to_string()
    };
    let reply = |content: Value, s: u32, more: Value| {
        let mut v = json!({ "type": "assistant", "timestamp": at(s), "message": { "content": content } });
        v.as_object_mut().unwrap().extend(more.as_object().unwrap().clone());
        v.to_string()
    };
    let lines = [
        reply(json!([{ "type": "text", "text": "Before anyone spoke." }]), 0, json!({})),
        person("<system-reminder>noise</system-reminder>Fix the login", 1, json!({})),
        reply(
            json!([{ "type": "text", "text": "Looking." }, { "type": "tool_use", "id": "t", "name": "Read", "input": { "file_path": "a.ts" } }]),
            2,
            json!({}),
        ),
        json!({ "type": "user", "timestamp": at(3), "message": { "content": [{ "type": "tool_result", "tool_use_id": "t", "content": "x" }] } }).to_string(),
        reply(json!([{ "type": "text", "text": "A subagent's." }]), 4, json!({ "isSidechain": true })),
        reply(json!([{ "type": "text", "text": "Fixed." }]), 5, json!({})),
        person(
            "<task-notification>\n<task-id>b1</task-id>\n<summary>Monitor \"PR #7\" ended</summary>\n<event>MERGED</event>\n</task-notification>",
            5,
            json!({ "origin": { "kind": "task-notification" } }),
        ),
        person("Thanks", 6, json!({})),
    ];
    std::fs::write(&file, lines.join("\n")).unwrap();
    let turns = claude_turns(&file);
    let shown: Vec<(Option<&str>, Vec<&str>)> = turns.iter().map(|t| (t.said.as_deref(), texts(t))).collect();
    assert_eq!(
        shown,
        [
            (
                Some("Fix the login"),
                vec!["Looking.", "Read: a.ts", "Fixed.", "Monitor \"PR #7\" ended · MERGED"]
            ),
            (Some("Thanks"), vec![]),
        ]
    );
    assert_eq!(turns[1].at - turns[0].at, 5000);
    let (picked, _) = pick_turns(&turns, Pick::default());
    assert_eq!(picked.len(), 2, "a message with no reply yet is kept");
}

#[test]
fn the_answer_is_turns_and_more() {
    let a = answer(&[turn(100, 1)], Pick::default());
    assert_eq!(
        a,
        json!({ "turns": [{ "at": 100, "events": [{ "at": 100, "kind": "text", "text": "100-0" }] }], "more": false })
    );
}
