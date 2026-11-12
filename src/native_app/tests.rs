use super::*;
use crate::test_support::{git, run};
use std::fs;
use std::path::PathBuf;

fn commit(dir: &Path, files: &[(&str, &str)], message: &str) -> String {
    for (name, text) in files {
        let path = dir.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    run(git(dir).args(["add", "-A"]));
    run(git(dir).args(["commit", "--quiet", "-m", message]));
    let out = git(dir).args(["rev-parse", "HEAD"]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn repo(files: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("repo");
    fs::create_dir(&dir).unwrap();
    run(git(&dir).args(["init", "--quiet", "-b", "main"]));
    run(git(&dir).args(["config", "user.email", "t@example.com"]));
    run(git(&dir).args(["config", "user.name", "T"]));
    commit(&dir, files, "first");
    (tmp, dir)
}

fn none() -> PathBuf {
    PathBuf::from("no-such-testflight.log")
}

#[test]
fn no_eas_json_is_no_native_app() {
    let (_t, dir) = repo(&[("app/app.json", "{}")]);
    assert_eq!(native_app(&dir, "main", &none(), &[]), None);
}

#[test]
fn app_json_names_its_platforms_and_version() {
    let (_t, dir) = repo(&[
        ("app/eas.json", "{}"),
        (
            "app/app.json",
            r#"{"expo":{"version":"1.2.3","ios":{"bundleIdentifier":"x"}}}"#,
        ),
    ]);
    let app = native_app(&dir, "main", &none(), &[]).unwrap();
    assert!(app.ios && !app.android);
    assert_eq!(app.version.as_deref(), Some("1.2.3"));
}

#[test]
fn app_config_js_is_read_as_text_and_an_eas_profile_adds_a_platform() {
    let (_t, dir) = repo(&[
        (
            "app/eas.json",
            r#"{"build":{"production":{"android":{"buildType":"apk"}}}}"#,
        ),
        (
            "app/app.config.js",
            "module.exports = { version: '4.5.6', ios: { x: 1 } };",
        ),
    ]);
    let app = native_app(&dir, "main", &none(), &[]).unwrap();
    assert!(app.ios && app.android);
    assert_eq!(app.version.as_deref(), Some("4.5.6"));
}

#[test]
fn a_config_naming_neither_platform_builds_for_neither() {
    let (_t, dir) = repo(&[("app/eas.json", "{}"), ("app/app.json", r#"{"expo":{}}"#)]);
    let app = native_app(&dir, "main", &none(), &[]).unwrap();
    assert!(!app.ios && !app.android && app.version.is_none() && app.changed_since.is_none());
}

#[test]
fn a_zero_platform_value_is_falsy_like_javascript() {
    assert!(!present(Some(&serde_json::json!(0))));
    assert!(!present(Some(&serde_json::json!(0.0))));
    assert!(present(Some(&serde_json::json!(1))));
    assert!(present(Some(&serde_json::json!({}))));
    let (_t, dir) = repo(&[
        ("app/eas.json", "{}"),
        ("app/app.json", r#"{"expo":{"android":0,"ios":{}}}"#),
    ]);
    let app = native_app(&dir, "main", &none(), &[]).unwrap();
    assert!(!app.android && app.ios);
}

fn log(tmp: &Path, text: &str) -> PathBuf {
    let file = tmp.join("testflight.log");
    fs::write(&file, text).unwrap();
    file
}

#[test]
fn changes_since_the_last_submitted_build_are_matched_to_items() {
    let (t, dir) = repo(&[("app/eas.json", "{}"), ("app/app.json", "{}")]);
    let built = commit(&dir, &[("app/a.txt", "1")], "built");
    commit(&dir, &[("server/x.txt", "1")], "not the app");
    let by_title = commit(&dir, &[("app/b.txt", "1")], "app-thing-piece-2: Something");
    let by_trailer = commit(&dir, &[("app/c.txt", "1")], "Tidy\n\nItem: other");
    let by_merge = commit(&dir, &[("app/d.txt", "1")], "Merge thing");
    let lines =
        format!("{{\"result\":\"submitted\",\"sha\":\"{built}\"}}\n{{\"result\":\"failed\",\"sha\":\"zzz\"}}\ntorn");
    let file = log(t.path(), &lines);
    let items = [
        ItemRef {
            id: "app-thing".into(),
            merge_commit: None,
        },
        ItemRef {
            id: "other".into(),
            merge_commit: None,
        },
        ItemRef {
            id: "merged".into(),
            merge_commit: Some(by_merge.clone()),
        },
    ];
    let app = native_app(&dir, "main", &file, &items).unwrap();
    let since = app.changed_since.unwrap();
    assert_eq!(since.sha, built);
    assert_eq!(since.paths, ["app/b.txt", "app/c.txt", "app/d.txt", "server/x.txt"]);
    let got: Vec<_> = since
        .changes
        .iter()
        .map(|c| (c.sha.clone(), c.item_id.clone()))
        .collect();
    assert_eq!(
        got,
        [
            (by_title, Some("app-thing".to_string())),
            (by_trailer, Some("other".to_string())),
            (by_merge, Some("merged".to_string())),
        ]
    );
    assert_eq!((since.more, since.more_changes), (None, None));
}

#[test]
fn more_than_100_paths_and_50_commits_are_counted() {
    let (t, dir) = repo(&[("app/eas.json", "{}")]);
    let built = commit(&dir, &[("app/base.txt", "1")], "built");
    for i in 0..55 {
        commit(&dir, &[(&format!("app/f{i}.txt"), "1")], &format!("change {i}"));
    }
    let many: Vec<(String, &str)> = (0..60).map(|i| (format!("app/more/{i}.txt"), "1")).collect();
    let refs: Vec<(&str, &str)> = many.iter().map(|(n, c)| (n.as_str(), *c)).collect();
    commit(&dir, &refs, "bulk");
    let file = log(t.path(), &format!("{{\"result\":\"submitted\",\"sha\":\"{built}\"}}\n"));
    let since = native_app(&dir, "main", &file, &[]).unwrap().changed_since.unwrap();
    assert_eq!((since.paths.len(), since.more), (100, Some(15)));
    assert_eq!((since.changes.len(), since.more_changes), (50, Some(6)));
    assert_eq!(since.changes[0].subject, "change 0");
}
