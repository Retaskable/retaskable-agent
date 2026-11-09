//! Every `gh` call for an item's work is guarded (`host-call-undefined-event`): an undefined argument is not run,
//! returns an error and leaves one entry in the sink.
#![cfg(unix)]
use std::path::Path;

use super::*;
use crate::host_call::{set_on_for, take_in};

/// A fake `gh` that logs its arguments to `ran` and prints `[]` (no open PR).
fn fake(dir: &Path) -> std::path::PathBuf {
    let gh = dir.join("fake_gh.sh");
    crate::test_support::write_executable(
        &gh,
        &format!("#!/bin/sh\necho \"$*\" >> '{}'\necho '[]'\n", dir.join("ran").display()),
    );
    let _ = std::fs::remove_file(dir.join("ran")); // `write_executable` may have run it
    gh
}

/// Runs `f` with the flag On for `dir`; checks the fake was not run, `f` erred and one `gh` entry holds `args`.
fn refused(f: impl FnOnce(&Path, &Path) -> anyhow::Result<()>, args: &[&str]) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().canonicalize().unwrap();
    let gh = fake(&dir);
    set_on_for(vec![dir.clone()]);
    let err = f(&gh, &dir).unwrap_err().to_string();
    set_on_for(vec![]);
    assert!(err.contains("argument"), "{err}");
    let ran = std::fs::read_to_string(dir.join("ran")).unwrap_or_default();
    assert!(!ran.contains(args[1]) || args[1] == "list", "the fake gh ran: {ran}");
    let got = take_in(&dir);
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].cmd, "gh");
    assert_eq!(got[0].args, args);
}

#[test]
fn open_or_create_refuses_an_empty_branch() {
    refused(
        |gh, d| publish::open_or_create(gh, d, "", "main", "t", "b").map(|_| ()),
        &["pr", "list", "--head", "", "--state", "open", "--json", "number,url"],
    );
}

#[test]
fn open_or_create_refuses_an_undefined_base() {
    refused(
        |gh, d| publish::open_or_create(gh, d, "feat", "undefined", "t", "b").map(|_| ()),
        &[
            "pr",
            "create",
            "--base",
            "undefined",
            "--head",
            "feat",
            "--title",
            "t",
            "--body-file",
            "-",
        ],
    );
}

#[test]
fn close_change_refuses_an_undefined_branch() {
    refused(
        |gh, d| publish::close_change(gh, d, "undefined", "c").map(|_| ()),
        &[
            "pr",
            "list",
            "--head",
            "undefined",
            "--state",
            "open",
            "--json",
            "number,url",
        ],
    );
}

#[test]
fn abandon_refuses_an_undefined_pr() {
    refused(
        |gh, d| abandon::abandon(Path::new("git"), gh, d, None, Some("undefined")),
        &["pr", "view", "undefined", "--json", "state", "--jq", ".state"],
    );
}

#[test]
fn merge_pr_refuses_an_undefined_argument() {
    refused(
        |gh, d| crate::drafts::land::merge_pr(gh, d, 7, "undefined", false),
        &["pr", "merge", "7", "undefined"],
    );
}

#[test]
fn with_the_flag_off_the_call_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().canonicalize().unwrap();
    let gh = fake(&dir);
    let _ = publish::close_change(&gh, &dir, "undefined", "c");
    assert!(std::fs::read_to_string(dir.join("ran")).unwrap().contains("pr list"));
    assert!(take_in(&dir).is_empty());
}
