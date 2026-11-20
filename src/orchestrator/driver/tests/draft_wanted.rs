//! The site's Add spec and Build this rows on a project's loop (`driver/draft_wanted.rs`): claimed in `claim` and in
//! `runs` mode, not while the project is paused (this computer's pause, or the store's) or the computer is signed out
//! of Claude, and finished with the new draft's id. The row is what the hub writes (`cloud/test/hubStoreCommands.test.ts`).
use super::asks::{Listed, finishes, ops_named, push, quiet_passes, store_failing, until};
use super::*;

fn add_spec() -> Value {
    json!({ "id": "in-1", "kind": "draft_wanted", "status": "requested", "issuedBy": "ana",
        "args": { "request": "Add a dark mode" } })
}

async fn rig_wanting() -> (Rig, Asked, Listed) {
    let (base, asked, listed) = store_failing("").await;
    let mut rig = rig(2, &[&base]);
    rig.go_live();
    rig.machine.wanted = true;
    rig.lp(0).wanted.poll_every = Duration::ZERO;
    push(&listed, add_spec());
    (rig, asked, listed)
}

fn claims(asked: &Asked) -> usize {
    ops_named(asked, "intent_claim").len()
}

#[tokio::test]
async fn add_spec_is_claimed_queued_as_a_draft_and_finished_with_its_id() {
    let (mut rig, asked, _) = rig_wanting().await;
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(sent[0]["id"], "in-1");
    assert_eq!(sent[0]["status"], "done");
    assert_eq!(sent[0]["result"], json!({ "id": "draft-new" }));
    let queued = ops_named(&asked, "draft_queue");
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0]["request"], "Add a dark mode");
    assert_eq!(queued[0]["requestedBy"], "ana");
    assert_eq!(queued[0]["assignee"], "ana");
    let lists = ops_named(&asked, "intent_list");
    // One list for every kind it takes: Add spec and Build this, the design presses and the reads.
    assert!(lists.iter().any(|a| a["kinds"]
        == json!([
            "design_draw_requested",
            "design_skip_requested",
            "draft_files",
            "draft_wanted",
            "environments",
            "issues_preview",
            "read_file",
            "release_status"
        ])));
}

#[tokio::test]
async fn it_is_taken_in_runs_mode_too() {
    let (mut rig, asked, _) = rig_wanting().await;
    rig.go_runs();
    let sent = rig.runs_until(&asked, 1).await;
    assert_eq!(sent[0]["status"], "done");
    assert_eq!(ops_named(&asked, "draft_queue").len(), 1);
}

#[tokio::test]
async fn a_paused_project_claims_none_until_it_is_resumed() {
    let (mut rig, asked, _) = rig_wanting().await;
    rig.pause("p0", true);
    quiet_passes(&mut rig).await;
    assert_eq!(claims(&asked), 0);
    assert!(ops_named(&asked, "draft_queue").is_empty());
    rig.pause("p0", false);
    until(&mut rig, &asked, 1).await;
    assert_eq!(claims(&asked), 1);
}

#[tokio::test]
async fn a_computer_signed_out_of_claude_claims_none_until_it_is_signed_in() {
    let (mut rig, asked, _) = rig_wanting().await;
    rig.machine.claude_signed_out_until = Some(std::time::Instant::now() + Duration::from_secs(3600));
    quiet_passes(&mut rig).await;
    assert_eq!(claims(&asked), 0);
    rig.machine.claude_signed_out_until = None;
    until(&mut rig, &asked, 1).await;
    assert_eq!(finishes(&asked).len(), 1);
}

#[tokio::test]
async fn several_rows_are_each_taken_and_finished() {
    let (mut rig, asked, listed) = rig_wanting().await;
    for n in 2..=3 {
        let mut row = add_spec();
        row["id"] = json!(format!("in-{n}"));
        push(&listed, row);
    }
    // Two run at once (`MAX_WORK_ROWS`); the third is taken on a later pass, none dropped.
    until(&mut rig, &asked, 3).await;
    assert_eq!(ops_named(&asked, "draft_queue").len(), 3);
}

/// A project whose checkout is a real repo with `README.md` on origin/main.
fn with_checkout(rig: &mut Rig) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let repo = crate::test_support::repo_with_origin(tmp.path())
        .to_string_lossy()
        .into_owned();
    // The loop works out its `project` from the file's each pass.
    rig.lp(0).file_project.repo_path = repo;
    rig.lp(0).file_project.base_branch = "main".into();
    tmp
}

#[tokio::test]
async fn a_read_row_is_answered_even_while_paused_and_finished_with_the_file() {
    let (mut rig, asked, listed) = rig_wanting().await;
    listed.rows.lock().unwrap().clear();
    let _repo = with_checkout(&mut rig);
    // `read_file { path }` as a row: it starts no work, so the pause that holds Add spec back does not hold it.
    push(
        &listed,
        json!({ "id": "in-9", "kind": "read_file", "status": "requested", "issuedBy": "ana",
            "args": { "path": "README.md" } }),
    );
    rig.pause("p0", true);
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(sent[0]["id"], "in-9");
    assert_eq!(sent[0]["status"], "done");
    assert_eq!(
        sent[0]["result"],
        json!({ "path": "README.md", "ref": "origin/main", "content": "hello" })
    );
    let lists = ops_named(&asked, "intent_list");
    assert!(lists.iter().any(|a| {
        a["kinds"]
            .as_array()
            .is_some_and(|k| k.iter().any(|k| k == "read_file"))
    }));
}

#[tokio::test]
async fn a_pushed_release_status_is_run_by_the_loop_and_answered_and_a_read_row_kind_is_not_a_command() {
    let (mut rig, _asked, listed) = rig_wanting().await;
    listed.rows.lock().unwrap().clear();
    let items = rig.machine.links.item_commands.clone();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    items.set_out("p0", Some(tx));
    for (id, kind) in [("c1", "release_status"), ("c2", "read_file")] {
        items.push_read(crate::view::ReadCommand {
            project: "p0".into(),
            command_id: id.into(),
            kind: kind.into(),
            args: json!({ "part": "saved" }),
        });
    }
    rig.machine.pass().await;
    let mut answers = Vec::new();
    for _ in 0..2 {
        answers.push(
            tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .unwrap()
                .unwrap(),
        );
    }
    answers.sort_by_key(|a| a["id"].as_str().unwrap().to_string());
    // The fake store has no saved notes to give: never a guessed list.
    assert_eq!(answers[0]["type"], "nack");
    assert_eq!(answers[0]["id"], "c1");
    assert!(
        answers[0]["reason"].as_str().unwrap().contains("shared backlog"),
        "{}",
        answers[0]
    );
    assert_eq!(answers[1]["id"], "c2");
    assert_eq!(answers[1]["type"], "nack");
}

#[tokio::test]
async fn a_push_naming_the_intents_lists_the_rows_at_once() {
    let (mut rig, asked, _listed) = rig_wanting().await;
    rig.lp(0).wanted.poll_every = Duration::from_secs(3600);
    until(&mut rig, &asked, 1).await;
    let before = ops_named(&asked, "intent_list").len();
    rig.machine.pass().await;
    assert_eq!(ops_named(&asked, "intent_list").len(), before, "an hour's poll waits");
    crate::backlog_socket::wake::wake(
        &rig.machine.links.work,
        "p0",
        &crate::backlog_socket::Push::FactsChanged {
            kinds: vec!["intents".into()],
        },
    );
    rig.machine.pass().await;
    assert_eq!(ops_named(&asked, "intent_list").len(), before + 1);
}

/// The draft step the presses are run by.
fn with_drafts(rig: &mut Rig) {
    rig.machine.drafts = Some(Arc::new(super::approve::Host::default()));
}

fn design_row(id: &str, kind: &str, args: Value) -> Value {
    json!({ "id": id, "kind": kind, "status": "requested", "issuedBy": "ana", "args": args })
}

#[tokio::test]
async fn skip_the_design_is_a_row_that_runs_the_stores_op_and_is_taken_while_paused() {
    let (mut rig, asked, listed) = rig_wanting().await;
    with_drafts(&mut rig);
    listed.rows.lock().unwrap().clear();
    push(
        &listed,
        design_row("in-5", "design_skip_requested", json!({ "draftId": "d1" })),
    );
    rig.pause("p0", true);
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(
        (sent[0]["id"].as_str(), sent[0]["status"].as_str()),
        (Some("in-5"), Some("done"))
    );
    assert_eq!(sent[0].get("result"), None);
    assert_eq!(ops_named(&asked, "draft_skip_design"), [json!({ "id": "d1" })]);
    assert!(ops_named(&asked, "draft_take").is_empty(), "a skip takes no lease");
}

#[tokio::test]
async fn skip_the_design_is_taken_in_runs_mode_too() {
    let (mut rig, asked, listed) = rig_wanting().await;
    with_drafts(&mut rig);
    listed.rows.lock().unwrap().clear();
    push(
        &listed,
        design_row("in-5", "design_skip_requested", json!({ "draftId": "d1" })),
    );
    rig.go_runs();
    let sent = rig.runs_until(&asked, 1).await;
    assert_eq!(sent[0]["status"], "done");
    assert_eq!(ops_named(&asked, "draft_skip_design").len(), 1);
}

#[tokio::test]
async fn try_again_is_a_row_that_starts_work_so_a_paused_project_leaves_it_open() {
    let (mut rig, asked, listed) = rig_wanting().await;
    with_drafts(&mut rig);
    listed.rows.lock().unwrap().clear();
    push(
        &listed,
        design_row("in-6", "design_draw_requested", json!({ "draftId": "d1" })),
    );
    rig.pause("p0", true);
    quiet_passes(&mut rig).await;
    assert_eq!(claims(&asked), 0, "left open for another computer");
    // Resumed: it is taken and runs the press. The fake store gives no draft to take, so the row is refused in the press's words.
    rig.pause("p0", false);
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(sent[0]["id"], "in-6");
    assert_eq!(sent[0]["status"], "refused");
    assert!(!sent[0]["why"].as_str().unwrap_or_default().is_empty(), "{}", sent[0]);
}

#[tokio::test]
async fn try_again_is_left_open_by_a_computer_signed_out_of_claude_and_a_skip_is_not() {
    let (mut rig, asked, listed) = rig_wanting().await;
    with_drafts(&mut rig);
    listed.rows.lock().unwrap().clear();
    push(
        &listed,
        design_row("in-6", "design_draw_requested", json!({ "draftId": "d1" })),
    );
    push(
        &listed,
        design_row("in-7", "design_skip_requested", json!({ "draftId": "d1" })),
    );
    rig.machine.claude_signed_out_until = Some(std::time::Instant::now() + Duration::from_secs(3600));
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(sent[0]["id"], "in-7", "only the skip is taken");
    quiet_passes(&mut rig).await;
    assert_eq!(claims(&asked), 1);
}

#[tokio::test]
async fn try_again_refused_while_claude_is_out_of_time_is_released_for_another_computer() {
    let (mut rig, asked, listed) = rig_wanting().await;
    with_drafts(&mut rig);
    listed.rows.lock().unwrap().clear();
    push(
        &listed,
        design_row("in-6", "design_draw_requested", json!({ "draftId": "d1" })),
    );
    rig.machine.engine_limits.hit(
        crate::engines::EngineId::Claude,
        std::time::Instant::now() + Duration::from_secs(1800),
    );
    for _ in 0..200 {
        rig.machine.pass().await;
        if !ops_named(&asked, "intent_release").is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let released = ops_named(&asked, "intent_release");
    assert_eq!(released.len(), 1, "{:?}", asked.lock().unwrap());
    assert_eq!(released[0]["id"], "in-6");
    assert!(released[0]["note"].as_str().is_some_and(|n| !n.is_empty()));
    assert!(finishes(&asked).is_empty(), "released, not finished");
}

#[tokio::test]
async fn each_look_says_why_this_computer_takes_nothing() {
    let (mut rig, asked, listed) = rig_wanting().await;
    with_drafts(&mut rig);
    listed.rows.lock().unwrap().clear();
    rig.pause("p0", true);
    quiet_passes(&mut rig).await;
    let seen = ops_named(&asked, "intent_seen");
    assert!(!seen.is_empty(), "told the store");
    assert_eq!(seen[0]["why"], "paused");
    assert!(seen[0]["machine"].as_str().is_some_and(|m| !m.is_empty()));
    rig.pause("p0", false);
    rig.machine.engine_limits.hit(
        crate::engines::EngineId::Claude,
        std::time::Instant::now() + Duration::from_secs(1800),
    );
    quiet_passes(&mut rig).await;
    let last = ops_named(&asked, "intent_seen").pop().unwrap();
    assert_eq!(last["why"], "out_of_time");
    assert!(
        last["until"]
            .as_i64()
            .is_some_and(|ms| ms > crate::telemetry::events::now_ms())
    );
    rig.machine.engine_limits.clear_claude();
    quiet_passes(&mut rig).await;
    let free = ops_named(&asked, "intent_seen").pop().unwrap();
    assert_eq!(free.get("why"), None, "{free}");
}

#[tokio::test]
async fn a_draft_files_row_is_taken_only_by_the_computer_that_holds_the_plan() {
    let (mut rig, asked, listed) = rig_wanting().await;
    with_drafts(&mut rig);
    listed.rows.lock().unwrap().clear();
    let tmp = tempfile::tempdir().unwrap();
    rig.lp(0).project.worktrees_dir = tmp.path().to_string_lossy().into_owned();
    rig.lp(0).file_project.worktrees_dir = tmp.path().to_string_lossy().into_owned();
    push(&listed, design_row("in-8", "draft_files", json!({ "draftId": "d1" })));
    quiet_passes(&mut rig).await;
    assert_eq!(claims(&asked), 0, "no worktree for the plan here: another computer's");
    // The plan's worktree is here now (no git needed: it only has to be one).
    std::fs::create_dir_all(crate::draft_worktree::path(tmp.path(), "d1").join(".git")).unwrap();
    for _ in 0..50 {
        rig.machine.pass().await;
        if claims(&asked) > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(claims(&asked), 1);
}
