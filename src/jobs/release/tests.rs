use std::sync::Mutex;

use serde_json::json;

use super::*;
use crate::backlog::ItemStatus;
use crate::config::ProjectType;
use crate::engines::engine::test_result;
use crate::release_notes::{Entry, MergedPr};

struct Fake(Mutex<Option<Result<Gathered, GatherError>>>, Mutex<Vec<Range>>);

impl Fake {
    fn new(r: Result<Gathered, GatherError>) -> Fake {
        Fake(Mutex::new(Some(r)), Mutex::new(Vec::new()))
    }
}

impl ReleaseGather for Fake {
    async fn gather(&self, range: &Range) -> Result<Gathered, GatherError> {
        self.1.lock().unwrap().push(range.clone());
        self.0.lock().unwrap().take().expect("one gather")
    }
}

fn job(args: serde_json::Value) -> JobSpec {
    JobSpec::from_json(&json!({ "kind": "release_notes", "day": "2026-11-17", "args": args })).unwrap()
}

fn args() -> serde_json::Value {
    json!({ "from": "2026-11-10T00:00:00.000Z", "to": "2026-11-17T00:00:00.000Z", "generatedAt": "2026-11-17T09:00:00.000Z" })
}

fn writer(cwd: &Path) -> Writer<'_> {
    Writer {
        template: TEMPLATE,
        project_type: ProjectType::Web,
        base_branch: "main",
        github_repo: "o/r",
        cwd,
        max_full_entries: 120,
        budget_usd: 1.0,
        model: "haiku".to_string(),
        local: None,
        engine: None,
        cursor: None,
        sandbox: false,
    }
}

fn entry(chore: bool, item: bool) -> Entry {
    Entry {
        key: "k".into(),
        item: item.then(|| crate::release_notes::ItemRef {
            id: "i".into(),
            title: "Charts".into(),
            status: ItemStatus::OnMain,
        }),
        fallback: "Add charts".into(),
        prs: vec![],
        commit: None,
        specs: vec![],
        chore,
    }
}

fn section(entries: Vec<Entry>) -> Section {
    Section {
        key: "spec:docs/specs/charts.md".into(),
        heading: "Charts".into(),
        entries,
    }
}

#[test]
fn the_args_need_a_range() {
    let a = ReleaseArgs::from_job(&job(args())).unwrap();
    assert_eq!(
        (a.from.as_str(), a.generated_at.as_str()),
        ("2026-11-10T00:00:00.000Z", "2026-11-17T09:00:00.000Z")
    );
    assert_eq!(ReleaseArgs::from_job(&job(json!({ "to": "2026-11-17" }))), None);
    assert_eq!(ReleaseArgs::from_job(&job(json!({ "from": " ", "to": "x" }))), None);
    assert_eq!(
        ReleaseArgs::from_job(&job(json!({ "from": "a", "to": "b" })))
            .unwrap()
            .generated_at,
        ""
    );
}

#[tokio::test]
async fn no_checkout_no_range_or_too_many_prs_are_refusals_that_run_no_session() {
    let cwd = std::env::temp_dir();
    let w = writer(&cwd);
    let none = Fake::new(Ok(Gathered {
        sections: vec![],
        total_prs: 0,
    }));
    let run = |_| async { panic!("no session") };
    assert_eq!(
        release_job(&job(args()), false, &none, &w, run).await,
        Outcome::refused(NO_CHECKOUT)
    );
    assert_eq!(
        release_job(&job(json!({})), true, &none, &w, run).await,
        Outcome::refused(NO_RANGE)
    );
    assert!(none.1.lock().unwrap().is_empty());
    let many = Fake::new(Err(GatherError::TooMany(too_many(200))));
    assert_eq!(
        release_job(&job(args()), true, &many, &w, run).await,
        Outcome::refused("More than 200 PRs merged in this range: pick a shorter one.")
    );
    let broke = Fake::new(Err(GatherError::Failed("gh pr list failed: rate limit".into())));
    assert_eq!(
        release_job(&job(args()), true, &broke, &w, run).await,
        Outcome::failed("The release_notes run failed: gh pr list failed: rate limit")
    );
}

#[tokio::test]
async fn nothing_merged_is_an_empty_note_and_only_chores_the_plain_list_with_no_session() {
    let cwd = std::env::temp_dir();
    let w = writer(&cwd);
    let run = |_| async { panic!("no session") };
    let empty = Fake::new(Ok(Gathered {
        sections: vec![],
        total_prs: 0,
    }));
    let Outcome::Done { result, .. } = release_job(&job(args()), true, &empty, &w, run).await else {
        panic!()
    };
    assert!(result.markdown.contains("Nothing"), "{}", result.markdown);
    assert_eq!(
        (result.model.as_str(), result.cost_usd, result.error.clone()),
        ("", 0.0, None)
    );
    assert_eq!(
        result.counts,
        CountsOut {
            prs: 0,
            items: 0,
            chores: 0
        }
    );
    assert_eq!(empty.1.lock().unwrap()[0].generated_at, "2026-11-17T09:00:00.000Z");

    let chores = Fake::new(Ok(Gathered {
        sections: vec![Section {
            key: "chores".into(),
            heading: "Chores".into(),
            entries: vec![entry(true, false)],
        }],
        total_prs: 3,
    }));
    let Outcome::Done { result, .. } = release_job(&job(args()), true, &chores, &w, run).await else {
        panic!()
    };
    assert_eq!((result.model.as_str(), result.error), ("", None));
    assert_eq!(
        result.counts,
        CountsOut {
            prs: 3,
            items: 0,
            chores: 1
        }
    );
}

#[tokio::test]
async fn a_range_with_something_for_the_readers_runs_the_writer_and_answers_its_note() {
    let cwd = std::env::temp_dir();
    let w = writer(&cwd);
    let g = Fake::new(Ok(Gathered {
        sections: vec![section(vec![entry(false, true)])],
        total_prs: 1,
    }));
    let res = json!({ "status": "ok", "intro": "Charts arrived.", "highlights": [], "entries": [] });
    let out = release_job(&job(args()), true, &g, &w, |spec| async move {
        assert!(spec.prompt.contains("o/r") && spec.prompt.contains("2026-11-10"));
        assert_eq!(spec.model, "haiku");
        Ok(test_result(true, "success", Some(res), None))
    })
    .await;
    let Outcome::Done { result, .. } = out else {
        panic!("{out:?}")
    };
    assert_eq!(
        (result.model.as_str(), result.cost_usd, result.error.as_deref()),
        ("haiku", 0.12, None)
    );
    assert_eq!(
        result.counts,
        CountsOut {
            prs: 1,
            items: 1,
            chores: 0
        }
    );
    let v = serde_json::to_value(&result).unwrap();
    assert_eq!(v["costUsd"], 0.12);
    assert!(v.get("error").is_none());
}

#[tokio::test]
async fn a_writer_that_fails_leaves_the_plain_list_with_its_reason() {
    let cwd = std::env::temp_dir();
    let w = writer(&cwd);
    let g = Fake::new(Ok(Gathered {
        sections: vec![section(vec![entry(false, true)])],
        total_prs: 1,
    }));
    let out = release_job(&job(args()), true, &g, &w, |_| async {
        Ok(test_result(false, "error_max_budget_usd", None, None))
    })
    .await;
    let Outcome::Done { result, .. } = out else { panic!() };
    assert_eq!(
        result.error.as_deref(),
        Some("Claude hit its $1 budget (spent $0.12), so this is the plain list of what's new.")
    );
    assert!(!result.markdown.is_empty());
    let g = Fake::new(Ok(Gathered {
        sections: vec![section(vec![entry(false, true)])],
        total_prs: 1,
    }));
    let out = release_job(&job(args()), true, &g, &w, |_| async {
        Ok(test_result(
            true,
            "success",
            Some(json!({ "status": "error", "error": "no facts" })),
            None,
        ))
    })
    .await;
    let Outcome::Done { result, .. } = out else { panic!() };
    assert_eq!(
        result.error.as_deref(),
        Some("Claude couldn't write them: no facts, so this is the plain list of what's new.")
    );
}

fn pr_json(number: u64, merged_at: &str, branch: &str) -> serde_json::Value {
    json!({ "number": number, "title": format!("Change {number}"), "url": format!("https://github.com/o/r/pull/{number}"),
        "body": "", "mergedAt": merged_at, "headRefName": branch, "mergeCommit": null })
}

use crate::git_bin::run_test_git as git;

/// A checkout with `origin/main` at a commit that has a spec, and a fake `gh` that answers `pr list` and `pr view`.
struct Repo {
    _tmp: tempfile::TempDir,
    dir: std::path::PathBuf,
    gh: std::path::PathBuf,
}

fn repo(prs: &[serde_json::Value], view: serde_json::Value) -> Repo {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("repo");
    std::fs::create_dir_all(dir.join("docs/specs")).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("docs/specs/charts.md"), "# Charts — Spec\n\n## One\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "Add charts"]);
    git(&dir, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    std::fs::write(tmp.path().join("list.json"), serde_json::to_string(prs).unwrap()).unwrap();
    std::fs::write(tmp.path().join("view.json"), view.to_string()).unwrap();
    let gh = tmp.path().join("gh");
    crate::test_support::write_executable(
        &gh,
        &format!(
            "#!/bin/sh\ncase \"$2\" in list) cat \"{}\";; view) cat \"{}\";; esac\n",
            tmp.path().join("list.json").display(),
            tmp.path().join("view.json").display()
        ),
    );
    Repo { _tmp: tmp, dir, gh }
}

fn gather_in<'a>(r: &'a Repo, items: &'a [Item], drafts: &'a [Draft], max_prs: u32) -> RepoGather<'a> {
    RepoGather {
        gh_bin: &r.gh,
        git_bin: Path::new("git"),
        repo_path: &r.dir,
        github_repo: "o/r",
        base_branch: "main",
        max_prs,
        items,
        drafts,
    }
}

fn charts_item() -> Item {
    serde_json::from_value(
        json!({ "id": "charts", "title": "Charts", "status": "on_main", "rank": 1,
        "specRefs": ["docs/specs/charts.md"], "prUrl": "https://github.com/o/r/pull/1" }),
    )
    .unwrap()
}

#[cfg(unix)]
#[tokio::test]
async fn the_repo_gather_groups_merged_prs_by_their_item_and_cuts_the_range_edges() {
    let r = repo(
        &[
            pr_json(1, "2026-11-12T10:00:00Z", "retaskable/charts-1"),
            pr_json(2, "2026-11-09T23:00:00Z", "other"),
            pr_json(3, "2026-11-17T00:00:00Z", "retaskable/x-1"),
        ],
        json!({ "files": [] }),
    );
    let items = [charts_item()];
    let range = ReleaseArgs::from_job(&job(args())).unwrap().range();
    let g = gather_in(&r, &items, &[], 200).gather(&range).await.unwrap();
    assert_eq!(g.total_prs, 2, "the one before `from` is cut, the one at `to` is kept");
    let headings: Vec<&str> = g.sections.iter().map(|s| s.heading.as_str()).collect();
    assert!(headings.contains(&"Charts"), "{headings:?}");
    assert!(g
        .sections
        .iter()
        .flat_map(|s| &s.entries)
        .any(|e| e.item.as_ref().is_some_and(|i| i.id == "charts")));
}

#[cfg(unix)]
#[tokio::test]
async fn more_than_the_limit_is_too_many_and_a_failing_gh_is_a_failure() {
    let prs: Vec<serde_json::Value> = (1..=3).map(|n| pr_json(n, "2026-11-12T10:00:00Z", "b")).collect();
    let r = repo(&prs, json!({ "files": [] }));
    let range = ReleaseArgs::from_job(&job(args())).unwrap().range();
    let err = gather_in(&r, &[], &[], 2).gather(&range).await.unwrap_err();
    assert_eq!(
        err,
        GatherError::TooMany("More than 2 PRs merged in this range: pick a shorter one.".to_string())
    );
    assert!(
        gather_in(&r, &[], &[], 3).gather(&range).await.is_ok(),
        "exactly at the limit is fine"
    );
    crate::test_support::write_executable(&r.gh, "#!/bin/sh\necho 'rate limited' >&2\nexit 1\n");
    let err = gather_in(&r, &[], &[], 3).gather(&range).await.unwrap_err();
    assert!(
        matches!(&err, GatherError::Failed(m) if m.contains("rate limited")),
        "{err:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_spec_drafts_change_is_filled_with_the_files_it_touches() {
    let r = repo(
        &[pr_json(5, "2026-11-12T10:00:00Z", "draft/x")],
        json!({ "files": [{ "path": "docs/specs/charts.md" }] }),
    );
    let draft: Draft =
        serde_json::from_value(json!({ "id": "draft-1", "prUrl": "https://github.com/o/r/pull/5" })).unwrap();
    let range = ReleaseArgs::from_job(&job(args())).unwrap().range();
    let g = gather_in(&r, &[], &[draft], 200).gather(&range).await.unwrap();
    assert_eq!(g.total_prs, 1);
    let _: Option<&MergedPr> = None;
    assert!(
        g.sections.iter().any(|s| s.heading.contains("Charts")),
        "{:?}",
        g.sections.iter().map(|s| &s.heading).collect::<Vec<_>>()
    );
}
