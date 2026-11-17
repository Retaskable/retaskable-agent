//! A claim pass while an item holds its merge turn: the turn is the item's own, so it is neither asked for as a piece's nor given back.
use super::*;

#[tokio::test]
async fn no_piece_merge_is_asked_for_while_an_item_holds_its_merge_turn() {
    let (mut rig, script) = setup(1).await;
    let starter = FakeRuns::new(&[RunKind::Merge]);
    // What the store answers a holder: its own pieceless turn.
    script.lock().unwrap().merges = VecDeque::from([turn("t1")]);
    rig.lp(0).turns.hold("a", "t1");
    let t0 = Instant::now();
    pass(&mut rig, &starter, t0).await;
    pass(&mut rig, &starter, t0 + Duration::from_secs(1)).await;
    {
        let s = script.lock().unwrap();
        assert!(s.args("merge_next").is_empty(), "{:?}", s.ops());
        assert!(s.args("merge_result").is_empty(), "the item's turn was given back");
    }
    assert!(rig.lp(0).turns.holds(), "the item keeps its turn");
}
