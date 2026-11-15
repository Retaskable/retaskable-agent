use super::*;

#[test]
fn only_the_person_who_started_a_session_drives_it() {
    assert_eq!(driver_refusal("octo", "octo"), None);
    assert!(driver_refusal("octo", "mona").unwrap().contains("Only octo"));
}

#[test]
fn a_title_is_the_first_line_cut_at_a_word() {
    assert_eq!(title("  Fix the login\nand more"), "Fix the login");
    let long = format!("{} tail", "word ".repeat(30));
    let t = title(&long);
    assert!(t.ends_with('…') && t.chars().count() <= 81, "{t}");
    assert!(!t.contains("tail"));
    let one = "a".repeat(100);
    assert_eq!(title(&one), format!("{}…", "a".repeat(80)));
}

#[test]
fn ids_and_branches_are_named_from_the_title() {
    assert_eq!(id_for("Fix the Login!", 1), "expert-fix-the-login");
    assert_eq!(id_for("Fix the Login!", 3), "expert-fix-the-login-3");
    assert_eq!(branch("expert-x", "Fix it"), "retaskable/expert-x-fix-it");
    assert!(is_expert("expert-a", None));
    assert!(is_expert("a", Some("expert")));
    assert!(!is_expert("a", Some("chore")));
}

#[test]
fn a_fresh_session_is_told_its_branch_and_the_message() {
    let p = first_prompt("retaskable/x", "main", "Do it").unwrap();
    assert!(p.contains("retaskable/x") && p.contains("`main`") && p.contains("2–4 suggested"));
    assert!(p.ends_with("Do it"));
}

#[test]
fn a_result_is_a_summary_and_a_question_only_when_it_has_words() {
    let r = parse(&json!({ "summary": " Done. ", "question": { "question": "  ", "options": [] } }));
    assert_eq!(
        r,
        Reply {
            summary: "Done.".into(),
            ask: None
        }
    );
    let r = parse(&json!({
        "summary": "Which?",
        "question": { "question": "Pick", "options": [
            { "label": "A", "detail": "first" }, { "label": " " }, { "label": "B" },
            { "label": "C" }, { "label": "D" }, { "label": "E" }
        ] }
    }));
    let q = r.ask.unwrap();
    assert_eq!(q.options.len(), 4);
    assert_eq!(q.options[0], ("A".into(), Some("first".into())));
    assert_eq!(q.options[1].0, "B");
    assert_eq!(
        answer_line(&Reply {
            summary: "Which?".into(),
            ask: Some(q)
        }),
        "Which?\nAsks: Pick"
    );
    assert_eq!(parse(&json!(null)), Reply::default());
}
