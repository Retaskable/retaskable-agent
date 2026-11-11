//! The model a piece run picks (docs/specs/agent/07.1-model-tiers.md "First tries are written on Sonnet").
use super::*;

fn build_piece(extra: Value) -> RunPiece {
    let mut p = json!({ "title": "t" });
    p.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    serde_json::from_value(p).unwrap()
}

fn model_of(piece: &RunPiece) -> &'static str {
    model_for(None, crate::tiers::OPUS, &piece_item(piece), is_retry(piece))
}

#[test]
fn a_retried_or_big_piece_keeps_opus() {
    assert_eq!(model_of(&build_piece(json!({}))), "sonnet");
    let failed = json!({ "attempts": [{ "kind": "build", "outcome": "failed" }] });
    assert_eq!(model_of(&build_piece(failed)), "opus");
    let limited = json!({ "attempts": [{ "kind": "build", "outcome": "limited" }] });
    assert_eq!(model_of(&build_piece(limited)), "sonnet");
    assert_eq!(model_of(&build_piece(json!({ "size": "XL" }))), "opus");
    let tidy: Chore = chore_of(&build_piece(json!({ "chore": { "kind": "tidy" } }))).unwrap();
    let item = piece_item(&build_piece(json!({})));
    assert_eq!(
        model_for(Some(&tidy), crate::tiers::OPUS, &item, true),
        crate::tiers::chore_tier()
    );
}
