//! The setup checks in hello.
use super::*;

#[tokio::test]
async fn hello_is_sent_again_when_the_setup_checks_change_and_not_when_a_rerun_is_equal() {
    changed_checks().await;
}

async fn changed_checks() {
    use crate::setup_check::{SetupCheck, SetupChecks};
    let signed_in = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = signed_in.clone();
    let probe: crate::setup_check::runner::Probe = Box::new(move || {
        let ok = flag.load(std::sync::atomic::Ordering::SeqCst);
        Box::pin(async move {
            let c = || SetupCheck::unknown("x", None);
            SetupChecks {
                git: c(),
                gh_installed: c(),
                gh: c(),
                claude: c(),
                claude_signed_in: if ok { SetupCheck::pass("in") } else { c() },
                cursor: c(),
                pi: None,
                codex: None,
                docker: None,
            }
        })
    });
    let mut v = view(&["one"]);
    Arc::get_mut(&mut v).unwrap().checks = Arc::new(crate::setup_check::SetupCheckRunner::with_probe(probe));
    Arc::get_mut(&mut v).unwrap().settings =
        crate::hello_settings::HelloSettings::new(std::path::PathBuf::new(), crate::env::Env::empty(), 0);
    let (url, _, mut rx) = cloud().await;
    let (dir, store) = linked(&url);
    let c = start_connector(&url, store, &v, dir.path());
    let mut a = next(&mut rx).await;
    let hello = read(&mut a).await;
    assert!(hello.get("checks").is_none());
    assert_eq!(read(&mut a).await["type"], "snapshot");
    v.checks.run_now().await;
    let hello = read_not_alive(&mut a).await;
    assert_eq!(hello["type"], "hello");
    assert_eq!(hello["checks"]["claudeSignedIn"]["ok"], Value::Null);
    v.checks.run_now().await; // equal: nothing sent
    signed_in.store(true, std::sync::atomic::Ordering::SeqCst);
    v.checks.run_now().await;
    let hello = read_not_alive(&mut a).await;
    assert_eq!(hello["type"], "hello");
    assert_eq!(hello["checks"]["claudeSignedIn"]["ok"], true);
    c.stop(Duration::from_secs(1)).await;
}
