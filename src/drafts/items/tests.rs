use super::*;
use serde_json::json;

fn raw(v: serde_json::Value) -> Vec<RawItem> {
    serde_json::from_value(v).unwrap()
}

#[test]
fn keeps_good_items_and_drops_bad_ids_repeats_the_revised_item_and_untitled_ones() {
    let items = checked_items(
        &raw(json!([
            { "id": "dark-mode", "title": "  Dark   mode ", "dependsOn": ["theme-base", "dark-mode", "Bad Id", "theme-base"],
              "specRefs": ["docs/specs/look.md section 3", " docs/specs/look.md "], "kind": "feature", "checking": "auto" },
            { "id": "Not A Slug", "title": "x" },
            { "id": "dark-mode", "title": "again" },
            { "id": "revised", "title": "the item it revises" },
            { "id": "no-title", "title": "  " },
            { "id": "export", "title": "Export", "kind": "fix" },
        ])),
        Some("revised"),
        false,
    );
    let got: Vec<_> = items.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(got, ["dark-mode", "export"]);
    assert_eq!(items[0].title, "Dark mode");
    assert_eq!(items[0].depends_on, ["theme-base"]);
    assert_eq!(items[0].spec_refs, ["docs/specs/look.md"]);
    assert_eq!(
        (items[0].kind.as_deref(), items[0].checking.as_deref()),
        (Some("feature"), Some("auto"))
    );
    // Not a tweak or a feature: no kind. No call on checking: a person's.
    assert_eq!(
        (items[1].kind.as_deref(), items[1].checking.as_deref()),
        (None, Some("manual"))
    );
}

#[test]
fn with_derive_an_item_with_no_checking_call_stays_without_one() {
    let raw = raw(
        json!([{ "id": "a-b", "title": "A" }, { "id": "c-d", "title": "C", "checking": "auto" }, { "id": "e-f", "title": "E", "checking": "manual", "checkingWhy": "A new screen" }]),
    );
    let got: Vec<_> = checked_items(&raw, None, true)
        .into_iter()
        .map(|i| (i.checking, i.checking_why))
        .collect();
    assert_eq!(
        got,
        [
            (None, None),
            (Some("auto".into()), None),
            (Some("manual".into()), Some("A new screen".into()))
        ]
    );
}

#[test]
fn the_plans_tier_fills_in_the_calls_the_drafter_left_and_routine_leaves_them() {
    let items = checked_items(
        &raw(json!([{ "id": "a-b", "title": "A" }, { "id": "c-d", "title": "C", "checking": "auto" }])),
        None,
        true,
    );
    let careful = with_derived_checking(&items, "tryfirst", &["it adds a feature".into(), "a screen".into()]);
    assert_eq!(
        (careful[0].checking.as_deref(), careful[0].checking_why.as_deref()),
        (Some("manual"), Some("it adds a feature; a screen"))
    );
    assert_eq!(
        careful[1].checking.as_deref(),
        Some("auto"),
        "the drafter's own call is never changed"
    );
    assert_eq!(with_derived_checking(&items, "routine", &["x".into()]), items);
}

#[test]
fn a_milestone_comes_from_the_theme_of_its_first_spec_and_never_from_outside_the_checkout() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("docs/specs")).unwrap();
    std::fs::write(
        dir.path().join("docs/specs/look.md"),
        "# Look\n\nTheme: Looks, Feel\n\nKind: new\n\n## One\n\nTheme: No\n",
    )
    .unwrap();
    let items = checked_items(
        &raw(json!([
            { "id": "a-b", "title": "A", "specRefs": ["docs/specs/look.md"] },
            { "id": "c-d", "title": "C", "specRefs": ["docs/specs/look.md"], "milestone": "Own" },
            { "id": "e-f", "title": "E", "specRefs": ["../look.md"] },
            { "id": "g-h", "title": "G", "specRefs": ["docs/specs/missing.md"] },
        ])),
        None,
        false,
    );
    let got: Vec<_> = with_milestones(items, dir.path())
        .into_iter()
        .map(|i| i.milestone)
        .collect();
    assert_eq!(got, [Some("Looks".into()), Some("Own".into()), None, None]);
}

#[test]
fn the_open_items_list_leaves_out_built_and_skipped_ones() {
    let item = |id: &str, status: ItemStatus, deps: &[&str]| Item {
        id: id.into(),
        title: format!("Title of {id}"),
        status,
        depends_on: deps.iter().map(ToString::to_string).collect(),
        ..Default::default()
    };
    let text = open_items_text(&[
        item("open-one", ItemStatus::Todo, &[]),
        item("on-branch", ItemStatus::OnBranch, &["open-one"]),
        item("built", ItemStatus::OnMain, &[]),
        item("live", ItemStatus::InProduction, &[]),
        item("gone", ItemStatus::Skipped, &[]),
    ]);
    assert!(
        text.ends_with("\n- open-one: Title of open-one\n- on-branch: Title of on-branch (depends on open-one)"),
        "{text}"
    );
    assert_eq!(open_items_text(&[item("built", ItemStatus::OnMain, &[])]), "");
}
