//! The `auto_merge` and `queue` lands: enabling GitHub's auto-merge, waiting for it holding the turn, and the fallback
//! for a repo with auto-merge turned off. A fake `gh` answers `pr view` from a queue of views (the last one repeats).
use super::*;

const OPEN: &str = r#"{"mergeable":"MERGEABLE","mergeStateStatus":"CLEAN","state":"OPEN"}"#;
const MERGED: &str = r#"{"mergeable":"UNKNOWN","mergeStateStatus":"UNKNOWN","state":"MERGED"}"#;
const CLOSED: &str = r#"{"mergeable":"UNKNOWN","mergeStateStatus":"UNKNOWN","state":"CLOSED"}"#;
const CONFLICTING: &str = r#"{"mergeable":"CONFLICTING","mergeStateStatus":"DIRTY","state":"OPEN"}"#;
const NOT_MERGEABLE: &str = r#"{"mergeable":"UNKNOWN","mergeStateStatus":"BLOCKED","state":"OPEN"}"#;
/// Open with auto-merge on.
const ON: &str = r#"{"mergeable":"MERGEABLE","mergeStateStatus":"CLEAN","state":"OPEN","autoMergeRequest":{"mergeMethod":"SQUASH"}}"#;
/// Open in GitHub's merge queue (no auto-merge request of its own).
const QUEUED: &str = r#"{"mergeable":"MERGEABLE","mergeStateStatus":"CLEAN","state":"OPEN","autoMergeRequest":null,"isInMergeQueue":true}"#;
const OFF: &str = "GraphQL: Pull request Auto merge is not allowed for this repository (enablePullRequestAutoMerge)";

/// A fake `gh`. `pr view` answers the first line of `views` and drops it unless it is the last; `pr merge … --auto`
/// is refused with the first line of `auto-errs` (and drops it) while there are any; any other `pr merge` is refused
/// with `refuse-<method>`'s text when that file exists. Every call is logged.
fn fake_auto_gh(dir: &Path) -> PathBuf {
    let path = dir.join("auto_gh.sh");
    write_executable(
        &path,
        r#"#!/bin/sh
d=$(dirname "$0")
echo "$*" >> "$d/calls.log"
case "$*" in
  *mergeCommit*) echo '{"mergeCommit":{"oid":"abc1234567890abcdef1234567890abcdef12345"}}';;
  "pr view "*)
    if [ "$(wc -l < "$d/views")" -gt 1 ]; then head -n 1 "$d/views"; tail -n +2 "$d/views" > "$d/views.t"; mv "$d/views.t" "$d/views"; else cat "$d/views"; fi;;
  "pr merge "*--auto)
    if [ -s "$d/auto-errs" ]; then head -n 1 "$d/auto-errs" >&2; tail -n +2 "$d/auto-errs" > "$d/auto-errs.t"; mv "$d/auto-errs.t" "$d/auto-errs"; exit 1; fi
    exit 0;;
  "pr merge "*)
    for m in squash rebase merge; do
      case " $* " in *" --$m "*) [ -f "$d/refuse-$m" ] && { cat "$d/refuse-$m" >&2; exit 1; };; esac
    done
    exit 0;;
esac
"#,
    );
    path
}

fn views(rig: &Rig, views: &[&str]) {
    set(rig.dir.path(), "views", &(views.join("\n") + "\n"));
}

fn auto_errs(rig: &Rig, errs: &[&str]) {
    set(rig.dir.path(), "auto-errs", &(errs.join("\n") + "\n"));
}

fn start(rig: &Rig) {
    MergeRuns::with(fake_auto_gh(rig.dir.path()), tgit, quick())
        .start(rig.request.clone())
        .unwrap();
}

async fn run(rig: &mut Rig) -> RunEnd {
    start(rig);
    tokio::time::timeout(Duration::from_secs(20), rig.ends.recv())
        .await
        .expect("the run ended")
        .unwrap()
}

async fn rig_with(settings: Value) -> Rig {
    let mut rig = rig().await;
    github(&mut rig, settings);
    rig
}

async fn auto_rig() -> Rig {
    rig_with(json!({ "land": "auto_merge" })).await
}

fn enables(rig: &Rig) -> Vec<String> {
    merges(rig).into_iter().filter(|c| c.ends_with("--auto")).collect()
}

fn handed_off(rig: &Rig) -> usize {
    ops(&rig.log, "merge_result")
        .iter()
        .filter(|b| b["args"] == json!({ "outcome": "handed_off", "token": "M1" }))
        .count()
}

#[tokio::test]
async fn auto_merge_is_turned_on_once_handed_off_and_waited_for_until_merged() {
    let mut rig = auto_rig().await;
    // The restart read, the read before landing, then reads while GitHub waits for the checks.
    views(&rig, &[OPEN, OPEN, ON, ON, MERGED]);
    let end = run(&mut rig).await;
    assert_eq!(end.outcome, RunOutcome::Merged);
    assert_eq!(
        end.result,
        json!({ "commit": "abc1234567890abcdef1234567890abcdef12345" })
    );
    assert_eq!(merges(&rig), vec!["pr merge 7 --squash --delete-branch --auto"]);
    assert_eq!(handed_off(&rig), 1);
    // Nothing merged by the agent: no beat with phase merging either.
    assert!(ops(&rig.log, "merge_beat").is_empty());
    rig.assert_turn_released();
}

#[tokio::test]
async fn a_queue_piece_is_handed_off_and_watched_like_auto_merge() {
    let mut rig = rig_with(json!({ "land": "queue" })).await;
    views(&rig, &[OPEN, OPEN, QUEUED, QUEUED, MERGED]);
    let end = run(&mut rig).await;
    assert_eq!(end.outcome, RunOutcome::Merged);
    assert_eq!(
        end.result,
        json!({ "commit": "abc1234567890abcdef1234567890abcdef12345" })
    );
    assert_eq!(merges(&rig), vec!["pr merge 7 --squash --delete-branch --auto"]);
    assert_eq!(handed_off(&rig), 1, "queued, never re-enabled");

    let mut rig = rig_with(json!({ "land": "queue" })).await;
    views(&rig, &[OPEN, OPEN, CLOSED]);
    assert!(matches!(run(&mut rig).await.outcome, RunOutcome::Failed(_)));
    assert_eq!(handed_off(&rig), 1);

    let mut rig = rig_with(json!({ "land": "queue" })).await;
    views(&rig, &[OPEN, OPEN, CONFLICTING]);
    assert_eq!(run(&mut rig).await.outcome, RunOutcome::Updated);

    // The fallback condition is the TypeScript agent's, whatever the land: turned off and mergeable merges directly.
    let mut rig = rig_with(json!({ "land": "queue" })).await;
    auto_errs(&rig, &[OFF, OFF]);
    views(&rig, &[OPEN, OPEN, OPEN]);
    assert_eq!(run(&mut rig).await.outcome, RunOutcome::Merged);
    assert!(
        merges(&rig).iter().any(|c| c == "pr merge 7 --squash"),
        "{:?}",
        merges(&rig)
    );
}

#[tokio::test]
async fn the_method_and_branch_deletion_follow_the_project_and_a_sync_uses_a_merge_commit() {
    let mut rig = rig_with(json!({ "land": "auto_merge", "mergeMethod": "rebase", "deleteBranch": false })).await;
    views(&rig, &[OPEN, OPEN, MERGED]);
    assert_eq!(run(&mut rig).await.outcome, RunOutcome::Merged);
    assert_eq!(merges(&rig), vec!["pr merge 7 --rebase --auto"]);

    let mut rig = auto_rig().await;
    into_sync(&mut rig, "squash");
    github(&mut rig, json!({ "land": "auto_merge", "mergeMethod": "squash" }));
    views(&rig, &[OPEN, OPEN, MERGED]);
    assert_eq!(run(&mut rig).await.outcome, RunOutcome::Merged);
    assert_eq!(merges(&rig), vec!["pr merge 7 --merge --delete-branch --auto"]);
}

#[tokio::test]
async fn already_on_counts_as_on_and_a_refusal_is_tried_again_with_the_hand_off_sent_once_it_is() {
    let mut rig = auto_rig().await;
    auto_errs(&rig, &["GraphQL: Pull request Auto merge is already enabled"]);
    views(&rig, &[OPEN, OPEN, MERGED]);
    assert_eq!(run(&mut rig).await.outcome, RunOutcome::Merged);
    assert_eq!(enables(&rig).len(), 1);
    assert_eq!(handed_off(&rig), 1);

    let mut rig = auto_rig().await;
    auto_errs(
        &rig,
        &[
            "GraphQL: Pull request is in clean status",
            "GraphQL: Pull request is in clean status",
        ],
    );
    views(&rig, &[OPEN, OPEN, OPEN, OPEN, ON, MERGED]);
    assert_eq!(run(&mut rig).await.outcome, RunOutcome::Merged);
    assert_eq!(enables(&rig).len(), 3);
    assert_eq!(handed_off(&rig), 1);
    assert_eq!(
        merges(&rig).len(),
        3,
        "no direct merge for a refusal that isn't about auto-merge being off"
    );
}

#[tokio::test]
async fn auto_merge_turned_off_while_waiting_is_turned_on_again() {
    let mut rig = auto_rig().await;
    views(&rig, &[OPEN, OPEN, ON, OPEN, MERGED]);
    assert_eq!(run(&mut rig).await.outcome, RunOutcome::Merged);
    assert_eq!(enables(&rig).len(), 2);
    assert_eq!(handed_off(&rig), 2);
}

#[tokio::test]
async fn a_closed_change_needs_a_person() {
    let mut rig = auto_rig().await;
    views(&rig, &[OPEN, OPEN, CLOSED]);
    let RunOutcome::Failed(why) = run(&mut rig).await.outcome else {
        panic!("closed should fail")
    };
    assert!(why.contains("closed without merging"), "{why}");
    assert_eq!(handed_off(&rig), 1);
}

#[tokio::test]
async fn a_conflict_while_waiting_brings_the_branch_up_to_date() {
    let mut rig = auto_rig().await;
    views(&rig, &[OPEN, OPEN, CONFLICTING]);
    // Nothing to merge in, so the update is a no-op push: `updated`, the piece goes to CI again.
    assert_eq!(run(&mut rig).await.outcome, RunOutcome::Updated);
    assert_eq!(enables(&rig).len(), 1);

    // A real conflict goes back to the implementer with the instruction.
    let mut rig = auto_rig().await;
    commit(&rig.repo, "feat.txt", "main's");
    trun(tgit(&rig.repo).args(["push", "-q", "origin", "main"]));
    views(&rig, &[ON, ON, CONFLICTING]);
    // Auto-merge is on already, so the run waits, sees the conflict and updates: a conflict this time.
    assert_eq!(run(&mut rig).await.outcome, RunOutcome::Conflict);
}

#[tokio::test]
async fn a_conflict_with_no_rounds_left_needs_a_person() {
    let mut rig = auto_rig().await;
    rig.request.context.config.max_conflict_rounds = 0;
    views(&rig, &[OPEN, OPEN, CONFLICTING]);
    let RunOutcome::Failed(why) = run(&mut rig).await.outcome else {
        panic!("conflict should fail")
    };
    assert!(why.contains("maxConflictRounds 0"), "{why}");
}

#[tokio::test]
async fn a_repo_with_auto_merge_off_is_merged_directly_once_github_says_mergeable() {
    let mut rig = rig_with(json!({ "land": "auto_merge", "mergeMethod": "merge" })).await;
    // Refused three times: at the hand-off, and on each of two reads, the first with GitHub not calling it mergeable.
    auto_errs(&rig, &[OFF, OFF, OFF]);
    views(&rig, &[OPEN, OPEN, NOT_MERGEABLE, OPEN]);
    set(
        rig.dir.path(),
        "refuse-merge",
        "GraphQL: Merge commits are not allowed on this repository",
    );
    set(
        rig.dir.path(),
        "refuse-squash",
        "GraphQL: Squash merging is not allowed on this repository",
    );
    let end = run(&mut rig).await;
    assert_eq!(end.outcome, RunOutcome::Merged);
    assert_eq!(
        end.result,
        json!({ "commit": "abc1234567890abcdef1234567890abcdef12345" })
    );
    // The project's method first, then squash and rebase in turn; `feat` isn't Retaskable's own branch, so it stays.
    let on = "pr merge 7 --merge --delete-branch --auto".to_string();
    assert_eq!(
        merges(&rig),
        vec![
            on.clone(),
            on.clone(),
            on,
            "pr merge 7 --merge".to_string(),
            "pr merge 7 --squash".to_string(),
            "pr merge 7 --rebase".to_string(),
        ]
    );
    assert_eq!(handed_off(&rig), 0, "auto-merge was never on");
}

#[tokio::test]
async fn auto_merge_off_and_not_mergeable_merges_nothing_directly_and_keeps_asking() {
    let mut rig = auto_rig().await;
    auto_errs(&rig, &[OFF, OFF, OFF, OFF]);
    views(&rig, &[OPEN, OPEN, NOT_MERGEABLE, NOT_MERGEABLE, NOT_MERGEABLE, CLOSED]);
    assert!(matches!(run(&mut rig).await.outcome, RunOutcome::Failed(_)));
    assert!(merges(&rig).iter().all(|c| c.ends_with("--auto")), "{:?}", merges(&rig));
    assert_eq!(enables(&rig).len(), 4);
}

#[tokio::test]
async fn the_turns_phase_while_waiting_is_auto_merge() {
    let mut rig = auto_rig().await;
    views(&rig, &[OPEN, OPEN, ON]);
    start(&rig);
    let locks = rig.request.context.locks.clone();
    tokio::time::timeout(Duration::from_secs(20), async {
        while locks.phase("M1") != "auto_merge" {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("the phase became auto_merge");
    // The ceiling or a stop ends the run, and the turn is given back for the store to hand out again.
    locks.apply_beat(&[("M1".to_string(), BeatVerdict::Stop)].into());
    let end = rig.ends.recv().await.unwrap();
    assert_eq!(end.outcome, RunOutcome::Interrupted);
}

#[tokio::test]
async fn a_store_that_cant_take_the_hand_off_ends_the_run_interrupted() {
    let mut rig = auto_rig().await;
    rig.request.context.client = Arc::new(crate::backlog::Client::new("http://127.0.0.1:9".to_string()));
    views(&rig, &[OPEN, OPEN, MERGED]);
    assert_eq!(run(&mut rig).await.outcome, RunOutcome::Interrupted);
}

#[tokio::test]
async fn a_run_started_again_reads_the_change_first_and_never_enables_or_pushes() {
    // Auto-merge is on and the base has moved: the change is waited for as it is.
    let mut rig = auto_rig().await;
    commit(&rig.repo, "base.txt", "base");
    trun(tgit(&rig.repo).args(["push", "-q", "origin", "main"]));
    let before = rig.origin_head("feat");
    views(&rig, &[ON, ON, MERGED]);
    let end = run(&mut rig).await;
    assert_eq!(end.outcome, RunOutcome::Merged);
    assert!(
        calls(rig.dir.path())[0].starts_with("pr view 7"),
        "{:?}",
        calls(rig.dir.path())
    );
    assert!(merges(&rig).is_empty());
    assert_eq!(rig.origin_head("feat"), before);

    // Merged while it was away: reported merged with its commit.
    let mut rig = auto_rig().await;
    views(&rig, &[MERGED]);
    let end = run(&mut rig).await;
    assert_eq!(
        end.result,
        json!({ "commit": "abc1234567890abcdef1234567890abcdef12345" })
    );
    assert!(merges(&rig).is_empty());
}

#[tokio::test]
async fn auto_merge_never_forces_or_pushes_to_the_base() {
    let mut rig = auto_rig().await;
    auto_errs(&rig, &[OFF]);
    views(&rig, &[OPEN, OPEN, OPEN, MERGED]);
    let before = rig.origin_head("main");
    assert_eq!(run(&mut rig).await.outcome, RunOutcome::Merged);
    assert_eq!(rig.origin_head("main"), before);
    let all = calls(rig.dir.path());
    assert!(
        all.iter().all(|c| c.starts_with("pr ") && !c.contains("force")),
        "{all:?}"
    );
}
