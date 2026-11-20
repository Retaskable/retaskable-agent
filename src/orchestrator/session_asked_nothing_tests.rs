use super::*;

#[test]
fn a_session_that_stops_to_ask_but_asks_nothing_is_carried_on_like_an_answered_question() {
    let r: ImplementerResult =
        serde_json::from_value(serde_json::json!({ "status": "needs_clarification", "summary": "s" })).unwrap();
    assert!(matches!(
        classify(&r),
        SessionResult::Implementer(ImplementerOutcome::Questions {
            all_already_answered: true
        })
    ));
}
