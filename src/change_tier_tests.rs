use serde_json::json;

use super::*;

fn file(path: &str, added: u32, text: Option<&str>) -> DiffFile {
    DiffFile {
        path: path.into(),
        added,
        removed: 0,
        added_text: text.map(str::to_string),
    }
}

fn signals(files: Vec<DiffFile>) -> DiffSignals {
    DiffSignals {
        files,
        ..Default::default()
    }
}

fn early() -> Choices {
    Choices::of(None)
}

fn routine() -> TierResult {
    TierResult {
        tier: Tier::Routine,
        reasons: vec![],
    }
}

#[test]
fn a_small_plain_diff_raises_nothing() {
    let d = signals(vec![file("src/lib.rs", 20, None), file("src/a.test.ts", 9, None)]);
    assert_eq!(escalate_tier(routine(), &d, &early()), routine());
}

#[test]
fn auth_secrets_and_commands_paths_need_you_but_a_test_of_them_does_not() {
    let d = signals(vec![
        file("agent/src/auth.ts", 5, None),
        file("app/.env.production", 1, None),
        file("agent/src/commands.ts", 1, None),
        file("agent/src/auth.test.ts", 5, None),
    ]);
    let r = escalate_tier(routine(), &d, &early());
    assert_eq!(r.tier, Tier::NeedsYou);
    assert_eq!(
        r.reasons,
        [
            "the change touches sign-in code (agent/src/auth.ts)",
            "the change touches secrets or deploy config (app/.env.production)",
            "the change touches what runs on a computer (agent/src/commands.ts)",
        ]
    );
}

#[test]
fn a_migration_is_try_first_when_add_only_and_needs_you_when_it_may_drop_or_is_unread() {
    let add = signals(vec![file(
        "db/migrations/1.sql",
        3,
        Some("+ALTER TABLE t ADD COLUMN x int;\n+CREATE INDEX i ON t(x);"),
    )]);
    let r = escalate_tier(routine(), &add, &early());
    assert_eq!(
        (r.tier, r.reasons),
        (Tier::TryFirst, vec!["a migration (db/migrations/1.sql)".to_string()])
    );
    for text in [
        Some("+DROP TABLE t;"),
        Some("+ALTER TABLE t DROP COLUMN x;"),
        Some("+alter table t rename to u;"),
        None,
    ] {
        let r = escalate_tier(
            routine(),
            &signals(vec![file("db/migrations/2.sql", 1, text)]),
            &early(),
        );
        assert_eq!(r.tier, Tier::NeedsYou, "{text:?}");
        assert_eq!(
            r.reasons,
            ["a migration that may drop or alter data (db/migrations/2.sql)"]
        );
    }
}

#[test]
fn a_new_dependency_is_try_first_and_the_profile_can_let_it_ship() {
    let d = DiffSignals {
        new_dependencies: true,
        ..Default::default()
    };
    let r = escalate_tier(routine(), &d, &early());
    assert_eq!(
        (r.tier, r.reasons),
        (Tier::TryFirst, vec!["it adds a dependency".to_string()])
    );
    let ship = RiskProfile::from_values(
        &json!({ "riskProfile": { "stage": "early", "choices": { "early": { "feat": "ship" } } } }),
    );
    assert_eq!(escalate_tier(routine(), &d, &Choices::of(ship.as_ref())), routine());
}

#[test]
fn a_big_diff_for_its_size_is_try_first_then_needs_you_and_one_with_no_size_is_try_first_at_most() {
    let lines = |n, size| DiffSignals {
        files: vec![file("src/a.rs", n, None)],
        size,
        ..Default::default()
    };
    // Early: big is ask (capped at try first when merely over), unk is try.
    assert_eq!(
        escalate_tier(routine(), &lines(150, Some(Size::S)), &early()),
        routine()
    );
    let r = escalate_tier(routine(), &lines(151, Some(Size::S)), &early());
    assert_eq!(
        (r.tier, r.reasons),
        (
            Tier::TryFirst,
            vec!["151 lines changed, more than a S piece".to_string()]
        )
    );
    let r = escalate_tier(routine(), &lines(451, Some(Size::S)), &early());
    assert_eq!(
        (r.tier, r.reasons),
        (
            Tier::NeedsYou,
            vec!["451 lines changed, far more than a S piece".to_string()]
        )
    );
    let r = escalate_tier(routine(), &lines(601, None), &early());
    assert_eq!(
        (r.tier, r.reasons),
        (
            Tier::TryFirst,
            vec!["601 lines changed and the piece has no size".to_string()]
        )
    );
}

#[test]
fn screens_and_navigation_follow_the_profile() {
    let d = signals(vec![
        file("app/src/screens/Home.tsx", 4, None),
        file("app/src/nav.ts", 4, None),
    ]);
    let r = escalate_tier(routine(), &d, &early());
    assert_eq!(r.tier, Tier::NeedsYou);
    assert_eq!(r.reasons.len(), 2, "{:?}", r.reasons);
    let proto = RiskProfile::from_values(&json!({ "riskProfile": { "stage": "prototype" } }));
    assert_eq!(escalate_tier(routine(), &d, &Choices::of(proto.as_ref())), routine());
}

#[test]
fn a_profile_reads_the_current_stages_changes_and_nothing_else_is_a_profile() {
    let v = json!({ "riskProfile": { "stage": "live", "choices": { "live": { "db": "ship", "bogus": "ask" }, "early": { "db": "ask" } } } });
    let p = RiskProfile::from_values(&v).unwrap();
    let m = signals(vec![file("db/migrations/1.sql", 1, Some("+CREATE TABLE t(a int);"))]);
    assert_eq!(escalate_tier(routine(), &m, &Choices::of(Some(&p))), routine());
    assert_eq!(escalate_tier(routine(), &m, &Choices::of(None)).tier, Tier::TryFirst);
    for bad in [
        json!({}),
        json!({ "riskProfile": null }),
        json!({ "riskProfile": { "stage": "huge" } }),
        json!({ "riskProfile": { "choices": {} } }),
    ] {
        assert_eq!(RiskProfile::from_values(&bad), None, "{bad}");
    }
}

#[test]
fn a_plan_tier_is_never_lowered_and_keeps_its_reasons_first() {
    let plan = TierResult {
        tier: Tier::NeedsYou,
        reasons: vec!["the plan flags a risk".into()],
    };
    let d = signals(vec![file("db/migrations/1.sql", 1, Some("+CREATE TABLE t(a int);"))]);
    assert_eq!(escalate_tier(plan.clone(), &d, &early()), plan);
    let plan = TierResult {
        tier: Tier::TryFirst,
        reasons: vec!["a people step".into()],
    };
    let d = signals(vec![file("src/auth/login.rs", 1, None)]);
    let r = escalate_tier(plan, &d, &early());
    assert_eq!(r.tier, Tier::NeedsYou);
    assert_eq!(r.reasons[0], "a people step");
}
