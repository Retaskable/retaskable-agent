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
    // One list for every kind it takes: Add spec and Build this, and the reads.
    assert!(
        lists
            .iter()
            .any(|a| a["kinds"] == json!(["draft_wanted", "issues_preview", "read_file"]))
    );
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
async fn a_pushed_read_is_run_by_the_loop_and_answered_with_its_result() {
    let (mut rig, _asked, listed) = rig_wanting().await;
    listed.rows.lock().unwrap().clear();
    let _repo = with_checkout(&mut rig);
    let items = rig.machine.links.item_commands.clone();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    items.set_out("p0", Some(tx));
    items.push_read(crate::view::ReadCommand {
        project: "p0".into(),
        command_id: "c1".into(),
        kind: "read_file".into(),
        args: json!({ "path": "README.md" }),
    });
    items.push_read(crate::view::ReadCommand {
        project: "p0".into(),
        command_id: "c2".into(),
        kind: "read_file".into(),
        args: json!({ "path": "../x.md" }),
    });
    rig.machine.pass().await;
    let mut answers = [
        tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap(),
        tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap(),
    ];
    answers.sort_by_key(|a| a["id"].as_str().unwrap().to_string());
    assert_eq!(
        answers[0],
        json!({ "type": "ack", "id": "c1", "result": { "path": "README.md", "ref": "origin/main", "content": "hello" } })
    );
    assert_eq!(
        answers[1],
        json!({ "type": "nack", "id": "c2", "reason": "Only Markdown files in the repo can be read." })
    );
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
