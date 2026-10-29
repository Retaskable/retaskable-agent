use super::*;

fn rules(lanes: u32) -> PoolRules {
    PoolRules {
        lanes,
        ..PoolRules::default()
    }
}

fn want(project: &str, item: &str) -> Want {
    Want {
        project: project.into(),
        item: item.into(),
        chore: false,
        last_lane: None,
        fix_queued: false,
    }
}

fn item(project: &str, item: &str) -> Holder {
    Holder::Item {
        project: project.into(),
        item: item.into(),
    }
}

#[test]
fn a_lane_is_handed_to_one_holder_at_a_time() {
    let mut pool = LanePool::new(rules(2));
    assert_eq!(pool.claim(&want("p", "A")), Some(1));
    assert_eq!(pool.claim(&want("p", "B")), Some(2));
    assert_eq!(pool.claim(&want("p", "C")), None, "both lanes are running work");
    assert!(!pool.take(1, item("p", "C")), "a held lane can't be taken again");
    assert!(!pool.release(1, &item("p", "B")), "only its holder gives it back");
    assert!(pool.release(1, &item("p", "A")));
    assert_eq!(pool.lane_of("p", "A"), None);
    assert_eq!(pool.claim(&want("p", "C")), Some(1));
    assert!(!pool.take(3, item("p", "D")), "past the computer's lanes");
    assert!(!pool.take(0, item("p", "D")));
}

#[test]
fn a_lane_another_project_holds_is_not_free() {
    let mut pool = LanePool::new(rules(2));
    assert_eq!(pool.claim(&want("game", "A")), Some(1));
    assert_eq!(
        pool.claim(&want("site", "A")),
        Some(2),
        "same item id, another project: its own lane"
    );
    assert_eq!(pool.holder(1).map(Holder::project), Some("game"));
    assert_eq!(pool.lane_of("site", "A"), Some(2));
}

#[test]
fn an_item_takes_its_last_lane_back_when_its_free() {
    let mut pool = LanePool::new(rules(3));
    let mut b = want("p", "B");
    b.last_lane = Some(3);
    assert_eq!(pool.claim(&b), Some(3), "its build is warm there");
    let mut c = want("p", "C");
    c.last_lane = Some(3);
    assert_eq!(pool.claim(&c), Some(1), "taken: the first free one instead");
    c.item = "D".into();
    c.last_lane = Some(9);
    assert_eq!(pool.claim(&c), Some(2), "a last lane past the pool is ignored");
}

#[test]
fn a_play_holds_its_lane_too() {
    let mut pool = LanePool::new(rules(1));
    let play = Holder::Play {
        project: "p".into(),
        item: "A".into(),
    };
    assert!(pool.take(1, play.clone()));
    assert_eq!(
        pool.claim(&want("p", "B")),
        None,
        "nothing else starts while the game is open"
    );
    assert!(pool.release(1, &play));
    assert_eq!(pool.claim(&want("p", "B")), Some(1));
}

#[test]
fn the_chore_lane_takes_chores_and_roadmap_items_only_when_no_fix_waits() {
    // logicLanes.test.ts: claimLane never lets a roadmap item take the chore lane ahead of a queued playtest fix.
    let mut pool = LanePool::new(PoolRules {
        chore_lane: Some(2),
        ..rules(2)
    });
    assert!(pool.take(1, item("p", "C")));
    let mut b = want("p", "B");
    b.fix_queued = true;
    assert_eq!(
        pool.claim_lane(&b),
        None,
        "lane 2 is free, but a playtest fix is queued for it"
    );
    b.fix_queued = false;
    assert_eq!(pool.claim_lane(&b), Some(2), "free to take once nothing waits for it");
    let mut chore = want("p", "chore-1");
    chore.chore = true;
    assert_eq!(pool.claim_lane(&chore), Some(2));
    assert!(pool.release(1, &item("p", "C")));
    assert_eq!(pool.claim_lane(&chore), Some(2), "a chore never takes a roadmap lane");
    assert_eq!(
        pool.free_lane(false, None, None, |_| true),
        Some(1),
        "new roadmap work never gets the chore lane first"
    );
}

#[test]
fn lane_scaling_narrows_new_work_to_the_base_lanes() {
    // logicLanes.test.ts: laneScaling narrows new work to the base lanes once the 5-hour window is used past its threshold.
    let scaled = |lanes, base, used: Option<f64>| {
        let mut pool = LanePool::new(PoolRules {
            scaling: Some(LaneScaling {
                base,
                below_used_pct: 80.0,
            }),
            ..rules(lanes)
        });
        pool.set_used_pct(used);
        pool.limit()
    };
    assert_eq!(LanePool::new(rules(4)).limit(), 4, "no laneScaling: always every lane");
    assert_eq!(scaled(4, 1, None), 4, "usage not known yet: every lane");
    assert_eq!(scaled(4, 1, Some(79.0)), 4, "under the threshold: every lane");
    assert_eq!(
        scaled(4, 1, Some(80.0)),
        1,
        "exactly at the threshold is no longer under it"
    );
    assert_eq!(scaled(4, 1, Some(100.0)), 1);
    assert_eq!(
        scaled(1, 3, Some(100.0)),
        1,
        "base never widens past the configured lanes"
    );
}

#[test]
fn running_work_keeps_a_lane_past_the_limit_and_a_cursor_lane_is_never_held_back() {
    let scaling = Some(LaneScaling {
        base: 1,
        below_used_pct: 50.0,
    });
    let mut pool = LanePool::new(PoolRules { scaling, ..rules(3) });
    assert_eq!(pool.claim(&want("p", "A")), Some(1));
    assert_eq!(pool.claim(&want("p", "B")), Some(2));
    pool.set_used_pct(Some(60.0));
    assert_eq!(
        pool.lane_of("p", "B"),
        Some(2),
        "work already in a higher lane carries on"
    );
    assert!(pool.release(1, &item("p", "A")));
    assert_eq!(
        pool.claim(&want("p", "C")),
        None,
        "lane 2 still counts against the base of 1"
    );
    assert!(pool.scaled_out(3));

    pool.set_rules(PoolRules {
        scaling,
        cursor_lanes: BTreeSet::from([3]),
        ..rules(3)
    });
    assert!(!pool.scaled_out(3));
    assert_eq!(
        pool.claim(&want("p", "C")),
        Some(3),
        "the 5-hour window is Claude Code's, not Cursor's"
    );
}

#[test]
fn free_lane_leaves_out_a_lane_whose_engine_cant_start() {
    let pool = LanePool::new(rules(2));
    assert_eq!(pool.free_lane(false, None, None, |k| k != 1), Some(2));
    assert_eq!(pool.free_lane(false, None, None, |_| false), None);
}

#[test]
fn the_merge_turn_gets_the_next_free_lane_ahead_of_everything_else() {
    // logicLanes.test.ts: the item holding the merge turn gets the next free lane to be checked again.
    let mut pool = LanePool::new(rules(2));
    assert!(pool.take(1, item("p", "C")));
    let mut h = want("p", "H");
    h.last_lane = Some(2);
    pool.set_merge_turn(Some(h.clone()));
    assert_eq!(pool.claim_lane(&want("p", "N")), None, "lane 2 is kept for H");
    assert_eq!(
        pool.free_lane(false, None, None, |_| true),
        None,
        "new work doesn't start in it either"
    );
    assert_eq!(pool.claim(&h), Some(2));
    assert!(pool.release(2, &item("p", "H")));
    assert_eq!(
        pool.claim_lane(&want("p", "N")),
        Some(2),
        "once H has had its lane, lanes are everyone's again"
    );
}

#[test]
fn a_lowered_lane_count_lets_work_in_a_higher_lane_finish() {
    let mut pool = LanePool::new(rules(3));
    assert!(pool.take(3, item("p", "A")));
    pool.set_rules(rules(1));
    assert_eq!(pool.lane_of("p", "A"), Some(3));
    assert!(pool.release(3, &item("p", "A")));
    assert!(!pool.take(3, item("p", "B")), "then it's left empty");
}

#[test]
fn dir_size_counts_files_but_not_git_or_a_linked_sibling() {
    let root = tempfile::tempdir().unwrap();
    let lane = root.path().join("lane-1");
    fs::create_dir_all(lane.join("target").join("debug")).unwrap();
    fs::write(lane.join("a.txt"), vec![0u8; 100]).unwrap();
    fs::write(lane.join("target").join("debug").join("b.bin"), vec![0u8; 1000]).unwrap();
    fs::create_dir_all(lane.join(".git")).unwrap();
    fs::write(lane.join(".git").join("index"), vec![0u8; 5000]).unwrap();
    let sibling = root.path().join("engine");
    fs::create_dir_all(&sibling).unwrap();
    fs::write(sibling.join("big.bin"), vec![0u8; 7000]).unwrap();
    crate::lanes::junction_or_symlink(&sibling, &root.path().join("lane-1").join("engine")).unwrap();
    assert_eq!(dir_size(&lane), 1100);
    assert_eq!(dir_size(&root.path().join("missing")), 0);
}

#[test]
fn lane_sizes_are_kept_per_project_and_heard_only_on_a_change() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("lane-2")).unwrap();
    fs::write(root.path().join("lane-2").join("x"), vec![0u8; 2_500_000_000 / 1000]).unwrap();
    let sizes = LaneSizes::default();
    let mut rx = sizes.subscribe();
    sizes.measure("p", root.path(), 2);
    assert!(rx.has_changed().unwrap());
    rx.mark_unchanged();
    assert_eq!(sizes.gb("p", 2), 0.0, "2.5 MB rounds to 0.00 GB");
    assert_eq!(
        rx.borrow().get("p").map(|m| m.keys().copied().collect::<Vec<_>>()),
        Some(vec![2]),
        "lane 1 isn't on disk"
    );
    sizes.measure("p", root.path(), 2);
    assert!(!rx.has_changed().unwrap(), "same sizes: nothing to send");
    sizes.set("q", BTreeMap::from([(1, 3.3)]));
    assert_eq!(sizes.gb("q", 1), 3.3);
    assert_eq!(sizes.gb("p", 1), 0.0);
}
