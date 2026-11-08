//! The snapshot sent again when the loop's live state changes.
use super::*;

#[tokio::test]
async fn a_change_in_the_loops_live_state_resends_the_snapshot_once_and_an_unchanged_tick_does_not() {
    let (url, _, mut rx) = cloud().await;
    let (dir, store) = linked(&url);
    let mut view = view(&["one"]);
    Arc::get_mut(&mut view).unwrap().settings = crate::hello_settings::HelloSettings::new(
        dir.path().to_path_buf(),
        crate::env::Env::empty(),
        4700,
        Arc::new(crate::flags::Flags::fixed(&["snapshot-reporting"])),
    );
    let c = start_connector(&url, store, &view, dir.path());
    let mut first = next(&mut rx).await;
    read(&mut first).await; // hello
    read(&mut first).await; // snapshot
    view.live.register("one");
    view.live.ticked("one", false, "t1".into(), vec![], "d".into());
    assert_eq!(read_not_alive(&mut first).await["type"], "snapshot");
    // A burst of changes sends one snapshot.
    view.live
        .set_limit("one", Some(std::time::Instant::now() + Duration::from_secs(60)));
    view.live.set_concurrency(2);
    view.live.ticked("one", true, "t2".into(), vec![], "d".into());
    assert_eq!(read_not_alive(&mut first).await["type"], "snapshot");
    assert!(
        tokio::time::timeout(Duration::from_millis(500), read_not_alive(&mut first))
            .await
            .is_err(),
        "one snapshot for the burst"
    );
    // A tick that changes nothing reported sends none.
    view.live.ticked("one", true, "t3".into(), vec![], "e".into());
    assert!(
        tokio::time::timeout(Duration::from_millis(500), read_not_alive(&mut first))
            .await
            .is_err(),
        "an unchanged tick"
    );
    c.stop(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn live_state_changes_send_nothing_while_snapshot_reporting_is_off() {
    let (url, _, mut rx) = cloud().await;
    let (dir, store) = linked(&url);
    let view = view(&["one"]);
    let c = start_connector(&url, store, &view, dir.path());
    let mut first = next(&mut rx).await;
    read(&mut first).await; // hello
    read(&mut first).await; // snapshot
    view.live.register("one");
    view.live.set_concurrency(2);
    assert!(
        tokio::time::timeout(Duration::from_millis(500), read_not_alive(&mut first))
            .await
            .is_err()
    );
    c.stop(Duration::from_secs(1)).await;
}
