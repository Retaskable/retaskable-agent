//! The `gh` and `glab` call sites refuse an undefined argument ([`crate::host_call::guard_program`]): the fake
//! program is not run, the site's own error comes back and the sink holds the entry.
use std::path::{Path, PathBuf};

use crate::host_call::Sink;
use crate::test_support::write_executable;

/// A program called `name` that leaves a `ran` file beside it when it runs.
pub(crate) fn fake(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    write_executable(&p, "#!/bin/sh\ntouch \"$(dirname \"$0\")/ran\"\necho '[]'\n");
    // `write_executable` runs it once to see it starts.
    let _ = std::fs::remove_file(dir.join("ran"));
    p
}

/// Sink empty.
pub(crate) fn on(_dir: &Path) {
    Sink::global().take();
}

/// Nothing ran, and the sink holds a refusal with this command and an argument containing `arg`.
pub(crate) fn refused(dir: &Path, cmd: &str, arg: &str) {
    assert!(!dir.join("ran").exists(), "the program ran");
    let got = Sink::global().take();
    assert!(
        got.iter()
            .any(|r| r.cmd == cmd && r.args.iter().any(|a| a.contains(arg))),
        "{got:?}"
    );
}

#[tokio::test]
async fn prs_list_refuses_an_empty_base() {
    let d = tempfile::tempdir().unwrap();
    let gh = fake(d.path(), "gh");
    on(d.path());
    let err = crate::agent_ci::prs::list(gh.as_os_str(), d.path(), "o/r", "").await;
    assert!(err.unwrap_err().contains("argument 8 was undefined"));
    refused(d.path(), "gh", "--limit");
}

#[tokio::test]
async fn automerge_refuses_an_empty_repo() {
    use crate::agent_ci::automerge::{Automerge, Ended};
    use crate::agent_ci::report::{Host, Kind};
    use crate::agent_ci::store::tests::FakeStore;
    let d = tempfile::tempdir().unwrap();
    let host = std::sync::Arc::new(Host {
        program: fake(d.path(), "gh"),
        kind: Kind::Github,
        repo: "undefined".into(),
        dir: d.path().to_path_buf(),
    });
    on(d.path());
    let ended = Automerge::new(FakeStore::winning()).run(&host, "main", 7, "h").await;
    assert!(
        matches!(&ended, Ended::Failed(m) if m.contains("was not run")),
        "{ended:?}"
    );
    // The refusal is recorded by the blocking thread that made the call, so only the program not running shows here.
    assert!(!d.path().join("ran").exists(), "the program ran");
}

#[test]
fn report_refuses_a_nan_sha_and_records_glab() {
    use crate::agent_ci::report::{Host, Kind, State};
    let d = tempfile::tempdir().unwrap();
    let h = Host {
        program: fake(d.path(), "glab"),
        kind: Kind::Gitlab,
        repo: "o/r".into(),
        dir: d.path().to_path_buf(),
    };
    on(d.path());
    let err = h.post_status("NaN", "ci/x", State::Pending, "d", None).unwrap_err();
    assert!(err.to_string().contains("glab was not run"), "{err}");
    refused(d.path(), "glab", "NaN");
}

#[test]
fn repo_calls_refuse_an_undefined_owner_for_every_project() {
    let d = tempfile::tempdir().unwrap();
    on(d.path());
    // The call has no directory: guarded while any project has the flag On.
    let err = crate::repo::list_repos(crate::repo::CodeHost::Github, "undefined").unwrap_err();
    assert!(format!("{err:#}").contains("gh was not run"), "{err:#}");
    refused(d.path(), "gh", "undefined");
}

#[test]
fn implementer_and_draft_calls_refuse_an_undefined_argument() {
    let d = tempfile::tempdir().unwrap();
    let gh = fake(d.path(), "gh");
    on(d.path());
    let err = crate::implementer::publish::close_change(&gh, d.path(), "", "c");
    assert!(err.unwrap_err().to_string().contains("was undefined"));
    refused(d.path(), "gh", "--head");
    let err = crate::implementer::abandon::abandon(&gh, &gh, d.path(), None, Some("undefined"));
    assert!(err.unwrap_err().to_string().contains("was undefined"));
    refused(d.path(), "gh", "undefined");
}

#[tokio::test]
async fn release_notes_gather_refuses_an_empty_repo() {
    let d = tempfile::tempdir().unwrap();
    let gh = fake(d.path(), "gh");
    on(d.path());
    let err = crate::release_notes::gather_merged_prs(&gh, d.path(), "", "main", "a", "b", 5).await;
    assert!(err.unwrap_err().to_string().contains("was undefined"));
    // The refusal is recorded by the blocking thread that made the call, so only the program not running shows here.
    assert!(!d.path().join("ran").exists(), "the program ran");
}
