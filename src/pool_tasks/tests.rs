use super::*;
use crate::backlog::pool::{FixSpec, JobSpec, SyncSpec};

fn task(id: &str, subject: &str) -> TaskInput {
    TaskInput {
        id: id.into(),
        subject: subject.into(),
        run: None,
        kind: None,
        sync: None,
        prompt: String::new(),
        tier: Tier::Smart,
        job: None,
        fix: None,
        extra_slot: false,
        order: u32::MAX,
    }
}

fn run_task(run: &str, subject: &str) -> TaskInput {
    TaskInput {
        run: Some(run.into()),
        ..task("t", subject)
    }
}

#[test]
fn a_task_is_what_its_run_says_whatever_its_subject() {
    // The subject is a lie on purpose: nothing is read from it when `run` is there.
    assert_eq!(
        classify(&run_task("item", "chore:tidy")),
        Kind::Item("chore:tidy".into())
    );
    let mut chore = run_task("chore", "a-1");
    chore.kind = Some("fix_main".into());
    assert_eq!(classify(&chore), Kind::Chore("fix_main".into()));
    chore.kind = Some("tidy".into());
    assert_eq!(classify(&chore), Kind::Chore("tidy".into()));
    let mut playtest = run_task("fix", "a-1");
    playtest.fix = Some(FixSpec {
        title: "T".into(),
        item_id: Some("a-1".into()),
        kind: Some("playtest".into()),
        key: Some("k".into()),
    });
    assert_eq!(classify(&playtest), Kind::Chore("fix_playtest".into()));
    playtest.fix.as_mut().unwrap().kind = Some("smoke".into());
    assert_eq!(classify(&playtest), Kind::Chore("fix_smoke".into()));
    let mut sync = run_task("sync", "x");
    sync.sync = Some(SyncSpec {
        branch: "dev".into(),
        main: "main".into(),
    });
    assert_eq!(classify(&sync), Kind::Chore("sync_main".into()));
    let mut job = run_task("job", "x");
    job.job = Some(JobSpec {
        kind: "lighthouse".into(),
        day: "2026-11-17".into(),
        args: None,
    });
    assert_eq!(classify(&job), Kind::Job("lighthouse".into()));
}

#[test]
fn a_run_this_agent_does_not_know_is_unknown_so_it_ends_not_ok() {
    assert_eq!(classify(&run_task("teleport", "a-1")), Kind::Unknown("teleport".into()));
    assert_eq!(
        classify(&run_task("chore", "a-1")),
        Kind::Unknown("chore".into()),
        "a chore with no kind"
    );
    assert_eq!(
        classify(&run_task("fix", "a-1")),
        Kind::Unknown("fix".into()),
        "a fix with no kind"
    );
    assert_eq!(
        classify(&run_task("job", "a-1")),
        Kind::Unknown("job".into()),
        "a job with no job"
    );
}

#[test]
fn a_task_with_no_run_from_an_older_store_is_read_from_its_subject() {
    assert_eq!(classify(&task("t1", "a-1")), Kind::Item("a-1".into()));
    assert_eq!(classify(&task("t3", "chore:tidy")), Kind::Chore("tidy".into()));
    assert_eq!(
        classify(&task("t5", "chore:sync_main:dev")),
        Kind::Chore("sync_main".into())
    );
    assert_eq!(
        classify(&task("t6", "fix_playtest:a-1@2026")),
        Kind::Chore("fix_playtest".into())
    );
    assert_eq!(
        classify(&task("t7", "fix_smoke:abc1234")),
        Kind::Chore("fix_smoke".into())
    );
    assert_eq!(
        classify(&task("t2", "chore:specs")),
        Kind::Unknown("specs".into()),
        "the store makes the specs draft itself now"
    );
    let mut job = task("t8", "job:lighthouse");
    job.job = Some(JobSpec {
        kind: "lighthouse".into(),
        day: "2026-11-17".into(),
        args: None,
    });
    assert_eq!(classify(&job), Kind::Job("lighthouse".into()));
}

#[test]
fn a_task_sent_again_is_not_taken_twice_but_a_forgotten_one_is_retried() {
    let mut runs = TaskRuns::default();
    let tasks = [task("t1", "a-1"), task("t2", "b-1")];
    assert_eq!(runs.fresh(&tasks).len(), 2);
    assert!(runs.fresh(&tasks).is_empty());
    runs.forget("t2");
    let again = runs.fresh(&tasks);
    assert_eq!(again.len(), 1);
    assert_eq!(again[0].id, "t2");
}

#[test]
fn a_restarted_computer_keeps_its_running_tasks_and_does_not_start_them_again() {
    let kept = BTreeMap::from([(
        "t1".to_string(),
        PoolTask {
            item: "a-1".into(),
            tier: "sonnet".into(),
        },
    )]);
    let mut runs = TaskRuns::restoring(kept);
    assert!(runs.fresh(&[task("t1", "a-1")]).is_empty());
    assert_eq!(runs.running().len(), 1);
}

#[test]
fn an_items_state_says_what_its_task_is() {
    use ItemState::*;
    for open in [Queued, Implementing, Checking, Reviewing] {
        assert_eq!(task_state(open), TaskState::Open);
    }
    for done in [AwaitingCi, Merging, AwaitingMerge, AwaitingPlaytest, Merged, Done] {
        assert_eq!(task_state(done), TaskState::Done);
    }
    for skipped in [NeedsClarification, NeedsHuman] {
        assert_eq!(task_state(skipped), TaskState::Skipped);
    }
}

#[test]
fn a_task_ends_when_its_item_leaves_the_sessions() {
    let mut runs = TaskRuns::default();
    runs.start("t1", "a-1", Tier::Smart);
    runs.start("t2", "b-1", Tier::Fast);
    runs.start("t3", "c-1", Tier::Smart);
    runs.start("t4", "d-1", Tier::Smart);
    let ends = runs.ended(|item| match item {
        "a-1" => Some(TaskState::Open),
        "b-1" => Some(TaskState::Done),
        "c-1" => Some(TaskState::Skipped),
        _ => None, // d-1 is gone
    });
    assert_eq!(
        ends,
        vec![
            TaskEnd::done("t2", true),
            TaskEnd::done("t3", false),
            TaskEnd::done("t4", false)
        ]
    );
    assert_eq!(runs.running().keys().collect::<Vec<_>>(), ["t1"]);
    assert!(runs.ended(|_| Some(TaskState::Open)).is_empty(), "an end is sent once");
}

#[test]
fn an_owed_end_goes_stale_after_a_day() {
    let owed = OwedTaskEnd {
        end: TaskEnd::done("t1", true),
        since: 100,
    };
    assert!(!owed.stale(100 + OWED_KEPT_SECS - 1));
    assert!(owed.stale(100 + OWED_KEPT_SECS));
    let back: OwedTaskEnd = serde_json::from_str(&serde_json::to_string(&owed).unwrap()).unwrap();
    assert_eq!(back, owed);
}
