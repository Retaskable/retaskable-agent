use super::*;
use crate::program_settings::{self, file_in};

fn obj(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

#[test]
fn a_repo_is_owner_slash_name_with_main_unless_it_says_otherwise() {
    let r = Repo::parse("Retaskable/Retaskable.com").unwrap();
    assert_eq!(
        (r.github_repo.as_str(), r.base_branch.as_str(), r.id().as_str()),
        ("Retaskable/Retaskable.com", "main", "Retaskable-Retaskable.com")
    );
    assert_eq!(Repo::parse("o/p@trunk").unwrap().base_branch, "trunk");
    assert_eq!(Repo::parse("o/p@trunk").unwrap().spec(), "o/p@trunk");
    assert_eq!(Repo::parse("o/p").unwrap().spec(), "o/p");
    for bad in ["p", "o/", "/p", "o/p/q", "o/p@", "o p/q", "../x"] {
        assert!(Repo::parse(bad).is_err(), "{bad}");
    }
}

#[test]
fn the_repo_list_is_comma_separated_and_drops_blanks_and_repeats() {
    let list = parse_repos(" a/b , ,c/d,a/b").unwrap();
    assert_eq!(
        list.iter().map(|r| r.github_repo.as_str()).collect::<Vec<_>>(),
        ["a/b", "c/d"]
    );
    assert!(parse_repos("a/b,nope").is_err());
}

#[test]
fn the_file_is_read_strictly_and_a_missing_one_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(read(dir.path()).unwrap(), File::default());
    let good = obj(json!({"repos": ["a/b", "c/d@dev", "a/b"], "slots": 3, "cpus": 4, "name": " box ", "other": 1}));
    let f = File::parse(&good).unwrap();
    assert_eq!(f.repos.len(), 2);
    assert_eq!((f.slots, f.cpus, f.name.as_deref()), (Some(3), Some(4), Some("box")));
    for bad in [
        json!({"repos": "a/b"}),
        json!({"repos": ["nope"]}),
        json!({"slots": "3"}),
        json!({"slots": 65}),
        json!({"cpus": 0}),
        json!({"name": ""}),
        json!({"name": "a\nb"}),
    ] {
        assert!(File::parse(&obj(bad.clone())).is_err(), "{bad}");
    }
}

#[test]
fn the_environment_wins_field_by_field_and_the_file_fills_the_rest() {
    let file = File {
        repos: parse_repos("f/one").unwrap(),
        slots: Some(5),
        cpus: Some(6),
        name: Some("filebox".into()),
    };
    let r = resolve(&Env::empty(), &file).unwrap();
    assert_eq!((r.slots, r.cpus, r.name.as_deref()), (5, 6, Some("filebox")));
    assert!(r.locked.is_empty());
    let env = Env::from_pairs([
        ("AGENT_CI_SLOTS", "0"),
        ("AGENT_CI_REPO", "e/two"),
        ("AGENT_CI_NAME", "envbox"),
    ]);
    let r = resolve(&env, &file).unwrap();
    assert_eq!((r.slots, r.cpus, r.name.as_deref()), (0, 6, Some("envbox")));
    assert_eq!(r.repos[0].github_repo, "e/two");
    assert_eq!(r.locked, ["repos", "slots", "name"]);
}

#[test]
fn defaults_apply_when_neither_says_and_no_repo_is_an_error() {
    let file = File {
        repos: parse_repos("o/p").unwrap(),
        ..File::default()
    };
    let r = resolve(&Env::empty(), &file).unwrap();
    assert_eq!((r.slots, r.cpus, r.name), (2, 8, None));
    assert!(resolve(&Env::empty(), &File::default()).is_err());
    let bad = Env::from_pairs([("AGENT_CI_SLOTS", "many")]);
    assert!(resolve(&bad, &file).is_err());
}

#[test]
fn a_save_writes_only_the_named_fields_and_keeps_the_rest_of_the_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(file_in(dir.path()), r#"{"slots":1,"keep":true}"#).unwrap();
    apply(
        dir.path(),
        &Env::empty(),
        &obj(json!({"repos": ["a/b@dev", "c/d"], "cpus": 4})),
    )
    .unwrap();
    let saved = program_settings::read_object(&file_in(dir.path())).unwrap();
    assert_eq!(saved["repos"], json!(["a/b@dev", "c/d"]));
    assert_eq!((saved["slots"].clone(), saved["keep"].clone()), (json!(1), json!(true)));
    // null and an empty list go back to the default.
    apply(dir.path(), &Env::empty(), &obj(json!({"repos": [], "cpus": null}))).unwrap();
    let saved = program_settings::read_object(&file_in(dir.path())).unwrap();
    assert!(!saved.contains_key("repos") && !saved.contains_key("cpus"));
}

#[test]
fn a_save_naming_a_locked_or_unknown_or_bad_field_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let env = Env::from_pairs([("AGENT_CI_SLOTS", "3")]);
    for bad in [
        json!({"cpus": 2, "slots": 9}),
        json!({"cpus": 2, "image": "x"}),
        json!({"cpus": 2, "name": ""}),
        json!({"cpus": 2, "repos": ["bad"]}),
    ] {
        assert!(apply(dir.path(), &env, &obj(bad.clone())).is_err(), "{bad}");
        assert!(!file_in(dir.path()).exists(), "{bad}");
    }
    let e = apply(dir.path(), &env, &obj(json!({"slots": 9}))).unwrap_err();
    assert!(e.0.contains("AGENT_CI_SLOTS"), "{e:?}");
}

#[test]
fn a_save_never_overwrites_a_file_that_is_not_json() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(file_in(dir.path()), "{oops").unwrap();
    assert!(apply(dir.path(), &Env::empty(), &obj(json!({"cpus": 2}))).is_err());
    assert_eq!(std::fs::read_to_string(file_in(dir.path())).unwrap(), "{oops");
}
