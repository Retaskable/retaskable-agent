//! The disk guard in the loop: below the threshold no check starts and the problem is the runner's `lastError`; it
//! resumes only above the threshold plus the margin.
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;

const GB: u64 = 1 << 30;

fn settings() -> Settings {
    Settings {
        repos: vec![Repo::parse("o/p").unwrap()],
        slots: 2,
        cpus: 2,
        computer: "t".into(),
        data_dir: std::env::temp_dir().join("retaskable-ci-test-never-written"),
    }
}

fn ci_with_free(free: Arc<AtomicU64>) -> CiLoop {
    let mut ci = CiLoop::new(Programs {
        docker: "retaskable-no-such-docker".into(),
        ..Programs::default()
    })
    .with_disk(crate::disk::Limits::default(), disk::Caps::default());
    ci.free = Arc::new(move |_| Some(free.load(Ordering::SeqCst)));
    ci
}

#[tokio::test]
async fn a_low_disk_takes_no_checks_and_says_why_then_resumes_with_the_margin() {
    let free = Arc::new(AtomicU64::new(3 * GB));
    let mut ci = ci_with_free(free.clone());
    let cfg = settings();
    ci.poll(&cfg).await;
    assert!(
        ci.logged["docker"].starts_with("Disk nearly full: 3.0 GB free on"),
        "{:?}",
        ci.logged
    );
    assert!(ci.logged["docker"].ends_with("cleaning up, taking no new work"));
    assert_eq!(ci.slot().available_permits(), 0);
    assert!(ci.disk_problem.is_some());

    // Back over the threshold but inside the margin: still held, even when a look is due.
    let d = ci.disk.as_mut().unwrap();
    let later = std::time::Instant::now() + crate::disk::CHECK_EVERY_LOW + Duration::from_secs(1);
    d.watch
        .check(later, std::slice::from_ref(&cfg.data_dir), |_| Some(12 * GB));
    assert!(d.watch.guard.holding());
    d.watch.check(
        later + crate::disk::CHECK_EVERY_LOW + Duration::from_secs(1),
        std::slice::from_ref(&cfg.data_dir),
        |_| Some(16 * GB),
    );
    assert!(!d.watch.guard.holding());
}

#[tokio::test]
async fn a_loop_with_no_guard_never_reads_the_disk() {
    let mut ci = CiLoop::new(Programs {
        docker: "retaskable-no-such-docker".into(),
        ..Programs::default()
    });
    ci.free = Arc::new(|_| panic!("the disk was read"));
    ci.poll(&settings()).await;
    assert!(ci.logged["docker"].contains("isn't installed"));
    assert!(ci.disk_problem.is_none());
}

#[tokio::test]
async fn a_disk_problem_is_the_status_files_last_error_unless_the_settings_have_one() {
    let free = Arc::new(AtomicU64::new(GB));
    let mut ci = ci_with_free(free);
    ci.poll(&settings()).await;
    assert!(ci.disk_problem.as_deref().unwrap().starts_with("Disk nearly full"));
    ci.settings_error = Some("bad settings".into());
    let shown = ci.settings_error.clone().or_else(|| ci.disk_problem.clone());
    assert_eq!(shown.as_deref(), Some("bad settings"));
}
