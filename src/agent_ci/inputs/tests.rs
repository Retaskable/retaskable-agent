use super::*;
use crate::agent_ci::report::Kind;

#[cfg(unix)]
fn host(dir: &std::path::Path, body: &str) -> Host {
    let program = dir.join("gh");
    crate::test_support::write_executable(&program, &format!("#!/bin/sh\n{body}\n"));
    Host {
        program,
        kind: Kind::Github,
        repo: "o/r".into(),
        dir: dir.to_path_buf(),
    }
}

#[cfg(unix)]
#[test]
fn the_token_is_the_login_and_no_login_is_an_error() {
    let d = tempfile::tempdir().unwrap();
    let h = host(d.path(), "[ \"$1 $2\" = \"auth token\" ] && echo ghp_abc");
    assert_eq!(h.token().unwrap(), "ghp_abc");
    let none = host(d.path(), "exit 1");
    assert!(none.token().unwrap_err().to_string().contains("no GitHub login"));
    let empty = host(d.path(), "true");
    assert!(empty.token().is_err());
}

#[cfg(unix)]
#[test]
fn the_files_are_listed_page_by_page_and_an_unreadable_list_is_empty() {
    let d = tempfile::tempdir().unwrap();
    let full: Vec<String> = (0..PER_PAGE).map(|i| format!("{{\"filename\":\"f{i}\"}}")).collect();
    std::fs::write(d.path().join("p1"), format!("[{}]", full.join(","))).unwrap();
    std::fs::write(d.path().join("p2"), r#"[{"filename":"docs/a.md"}]"#).unwrap();
    let h = host(d.path(), "case \"$4\" in *page=1) cat p1;; *page=2) cat p2;; esac");
    let files: Vec<String> = serde_json::from_str(&h.pr_files(7)).unwrap();
    assert_eq!(files.len(), PER_PAGE + 1);
    assert_eq!((files[0].as_str(), files[PER_PAGE].as_str()), ("f0", "docs/a.md"));
    assert_eq!(host(d.path(), "echo nope").pr_files(7), "[]");
    assert_eq!(host(d.path(), "exit 1").pr_files(7), "[]");
}

#[cfg(unix)]
#[test]
fn only_an_agent_branch_has_a_card_and_an_unreadable_body_is_none() {
    let d = tempfile::tempdir().unwrap();
    let body = r#"{"body":"Intro\r\nRelease flag: new-thing (On)\r\nmore"}"#;
    std::fs::write(d.path().join("pr.json"), body).unwrap();
    let h = host(d.path(), "cat pr.json");
    assert_eq!(h.card_line(7, "fix").as_deref(), Some(""));
    assert_eq!(h.card_line(0, "retaskable/x").as_deref(), Some(""));
    assert_eq!(
        h.card_line(7, "retaskable/x").as_deref(),
        Some("Release flag: new-thing (On)")
    );
    std::fs::write(d.path().join("pr.json"), r#"{"body":"no card"}"#).unwrap();
    assert_eq!(h.card_line(7, "retaskable/x").as_deref(), Some(""));
    std::fs::write(d.path().join("pr.json"), r#"{"body":null}"#).unwrap();
    assert_eq!(h.card_line(7, "retaskable/x"), None);
    assert_eq!(host(d.path(), "exit 1").card_line(7, "retaskable/x"), None);
}
