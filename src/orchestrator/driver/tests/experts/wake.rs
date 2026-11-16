//! The store socket's `facts_changed` naming `expert_queue` (backlog_socket/wake.rs): the queue is read back at once,
//! not at the minute's beat.
use super::*;
use crate::work_signal::Wake;

fn queued_from_site(x: &Ex) -> bool {
    x.expert(ID).is_some_and(|e| {
        e["queue"]
            .as_array()
            .is_some_and(|q| q.len() == 1 && q[0]["text"] == "from the site")
    })
}

#[tokio::test]
async fn a_push_reads_the_queue_back_before_the_beat_is_due() {
    let mut x = ex().await;
    x.host.hold.store(true, Ordering::SeqCst);
    x.start("s1", "Fix it");
    x.until("the turn running", |x| x.jobs().len() == 1).await;
    // The first beat has run; the next is a minute off.
    x.quiet().await;
    x.st.site_says(ID, "from the site");
    x.quiet().await;
    assert!(!queued_from_site(&x), "the beat read it before it was due");
    // Another project's wake, or another kind, changes nothing.
    x.rig.machine.links.work.wake_for("p1", Wake::ExpertQueue);
    x.rig.machine.links.work.wake_for("p0", Wake::Work);
    x.quiet().await;
    assert!(!queued_from_site(&x));
    x.rig.machine.links.work.wake_for("p0", Wake::ExpertQueue);
    x.until("the site's row", queued_from_site).await;
    // A wake is used up by the read it caused.
    x.st.site_says(ID, "again");
    x.quiet().await;
    assert_eq!(x.expert(ID).unwrap()["queue"].as_array().unwrap().len(), 1);
    x.host.hold.store(false, Ordering::SeqCst);
    x.host.go.notify_one();
}
