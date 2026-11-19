//! Rotation chores and `run_take` (split from runs.rs).
use super::*;

fn chore_run(token: &str, kind: &str) -> Value {
    let mut r = run(token, kind);
    r["piece"]["chore"] = json!({ "kind": token });
    r
}

#[tokio::test]
async fn chores_off_is_sent_and_a_stray_rotation_chore_is_given_back_but_fix_main_runs() {
    let (mut rig, script) = setup(2).await;
    rig.lp(0).project.chores = crate::config::OnOff::Off;
    rig.lp(0).file_project.chores = crate::config::OnOff::Off;
    let starter = FakeRuns::new(&[RunKind::Build]);
    script.lock().unwrap().takes = VecDeque::from([chore_run("tidy", "build"), chore_run("fix_main", "build")]);
    pass(&mut rig, &starter, Instant::now()).await;

    let s = script.lock().unwrap();
    assert_eq!(s.args("run_take")[0]["agent"]["chores"], json!(false));
    let done = s.args("run_done");
    assert_eq!(done.len(), 1);
    assert_eq!(done[0]["outcome"], "interrupted");
    assert_eq!(starter.started(), vec!["fix_main"], "the tidy chore ran no session");
}

#[tokio::test]
async fn chores_on_sends_no_key_and_the_stores_count_is_kept_for_the_status() {
    let (mut rig, script) = setup(1).await;
    let starter = FakeRuns::new(&[RunKind::Build]);
    let mut r = chore_run("tidy", "build");
    r["chores"] = json!({ "today": 2, "limit": 3 });
    script.lock().unwrap().takes = VecDeque::from([r]);
    let id = rig.machine.projects[0].id.clone();
    assert_eq!(rig.live.snapshot().projects[&id].chores, None);
    pass(&mut rig, &starter, Instant::now()).await;

    assert!(
        script.lock().unwrap().args("run_take")[0]["agent"]
            .get("chores")
            .is_none()
    );
    assert_eq!(starter.started(), vec!["tidy"]);
    let c = rig.live.snapshot().projects[&id].chores.unwrap();
    assert_eq!((c.today, c.limit), (2, 3));
}
