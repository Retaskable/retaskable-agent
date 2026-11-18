use super::*;

#[test]
fn a_turn_asks_for_at_most_four_helpers_each_with_words_a_lane_only_when_true() {
    let got = asked(&json!({ "helpers": [
        { "task": " look at a " }, { "task": "  " }, { "task": "edit b", "lane": true }, { "lane": true },
        { "task": "c", "lane": "yes" }, { "task": "d" }, { "task": "e" }
    ] }));
    let shown: Vec<(&str, bool)> = got.iter().map(|h| (h.task.as_str(), h.lane)).collect();
    assert_eq!(
        shown,
        [("look at a", false), ("edit b", true), ("c", false), ("d", false)]
    );
    assert!(asked(&json!({})).is_empty() && asked(&json!({ "helpers": "x" })).is_empty());
    assert_eq!(
        asked(&json!({ "helpers": [{ "task": "x".repeat(5000) }] }))[0]
            .task
            .chars()
            .count(),
        4000
    );
}

#[test]
fn names_and_start_lines_are_what_the_app_reads() {
    assert_eq!(name(2), "helper-2");
    assert_eq!(branch("retaskable/e-x", 2), "retaskable/e-x-h2");
    assert_eq!(started(1, "find it"), "Helper 1 started: find it");
    assert_eq!(what(3, "find it"), "Helper 3 (\"find it\")");
    let long = "w ".repeat(100);
    assert!(what(1, &long).chars().count() < 100);
}

#[test]
fn a_read_helpers_end_is_its_result_or_why_not_and_a_stop_tells_the_session_nothing() {
    let (note, told) = end_lines(1, "t", &Ended::Finished(" Found it in a.rs:3 ".into()), None);
    assert_eq!(note, "Helper 1 (\"t\") finished: Found it in a.rs:3");
    assert_eq!(told.as_deref(), Some(note.as_str()));
    let (note, told) = end_lines(1, "t", &Ended::Failed("it crashed".into()), None);
    assert_eq!(
        (note.as_str(), told.is_some()),
        ("Helper 1 (\"t\") didn't finish: it crashed", true)
    );
    assert_eq!(
        end_lines(1, "t", &Ended::Stopped, None),
        ("Helper 1 (\"t\") was stopped".into(), None)
    );
}

#[test]
fn a_lane_helpers_end_names_its_branch_whole_even_when_the_note_is_cut() {
    let result = "r ".repeat(2000);
    let (note, told) = end_lines(2, "t", &Ended::Finished(result), Some("b-h2"));
    assert!(
        note.chars().count() <= NOTE_MAX
            && note.ends_with("on b-h2, from your last commit: merge it in if you want them."),
        "{note}"
    );
    assert!(note.contains("\n\nIts commits are on b-h2"));
    let told = told.unwrap();
    assert!(told.starts_with("Helper 2 (\"t\") finished: r r") && told.ends_with("merge it in if you want them."));
    let (_, told) = end_lines(2, "t", &Ended::Failed("x".into()), Some("b-h2"));
    assert!(told.unwrap().contains("Anything it committed is on b-h2"));
    let (note, _) = end_lines(2, "t", &Ended::Stopped, Some("b-h2"));
    assert_eq!(note, "Helper 2 (\"t\") was stopped: anything it committed is on b-h2");
}

#[test]
fn the_prompts_name_the_branch_the_session_and_the_task() {
    let read = prompt("find it", "retaskable/e-x", None).unwrap();
    assert!(read.contains("on its branch `retaskable/e-x`") && read.trim_end().ends_with("find it"));
    let lane = prompt("edit it", "retaskable/e-x-h1", Some("retaskable/e-x")).unwrap();
    assert!(
        lane.contains("`retaskable/e-x-h1`, made from the session's branch `retaskable/e-x`")
            && lane.trim_end().ends_with("edit it")
    );
    assert_eq!(result_of(&json!({ "result": " a " })).as_deref(), Some("a"));
    assert_eq!(result_of(&json!({ "result": " " })), None);
    assert_eq!(schema()["required"][0], "result");
}
