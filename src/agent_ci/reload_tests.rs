//! The runner reads its settings again each poll (agent-ci.md section 3.7): a change applies, a bad file keeps the
//! old values and shows as `lastError` in the status file.
use super::*;
use crate::program_status;

fn settings(slots: u32) -> Settings {
    Settings {
        repos: vec![Repo::parse("o/p").unwrap()],
        slots,
        cpus: 2,
        computer: "t".into(),
        data_dir: std::env::temp_dir().join("retaskable-ci-test-never-written"),
    }
}

#[test]
fn a_new_read_replaces_the_settings_and_a_failed_one_keeps_them_until_one_works() {
    let mut ci = CiLoop::new(Programs::default());
    let mut cfg = settings(2);
    ci.reload(&mut cfg, || Ok(settings(5)));
    assert_eq!(cfg.slots, 5);
    ci.reload(&mut cfg, || anyhow::bail!("settings.json isn't valid JSON"));
    assert_eq!(cfg.slots, 5, "the old values stay");
    assert!(ci.settings_error.as_deref().unwrap().contains("valid JSON"));
    assert!(ci.logged["settings"].contains("keeping the old settings"));
    ci.reload(&mut cfg, || Ok(settings(1)));
    assert_eq!((cfg.slots, ci.settings_error.is_none()), (1, true));
    assert!(!ci.logged.contains_key("settings"));
}

#[test]
fn a_bad_file_is_the_last_error_in_the_status_file_and_a_good_one_clears_it() {
    let dir = tempfile::tempdir().unwrap();
    let writer = StatusWriter::new(dir.path(), program_status::CI_RUNNER, "1.0.0", 0);
    let mut ci = CiLoop::new(Programs::default()).with_status(writer, Arc::new(|| true), vec![]);
    let mut cfg = settings(2);
    ci.reload(&mut cfg, || anyhow::bail!("slots must be a whole number from 0 to 64."));
    ci.write_status(&cfg);
    let status = program_status::read(dir.path()).unwrap();
    assert!(status.last_error.unwrap().contains("slots must be"));
    ci.reload(&mut cfg, || Ok(settings(2)));
    ci.write_status(&cfg);
    assert_eq!(program_status::read(dir.path()).unwrap().last_error, None);
}
