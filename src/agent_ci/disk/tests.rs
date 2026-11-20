use super::*;
use crate::agent_ci::settings::Repo;

fn cfg(dir: &Path, repos: &[&str]) -> Settings {
    Settings {
        repos: repos.iter().map(|r| Repo::parse(r).unwrap()).collect(),
        slots: 1,
        cpus: 1,
        computer: "My PC".into(),
        data_dir: dir.to_path_buf(),
    }
}

#[test]
fn the_container_prune_is_scoped_to_this_runners_label_and_to_the_stopped_and_old() {
    let a = container_prune_args("My PC");
    assert_eq!(&a[..3], ["container", "prune", "-f"]);
    assert!(a.windows(2).any(|w| w == ["--filter", "until=1h"]));
    assert!(
        a.windows(2)
            .any(|w| w == ["--filter", "label=org.retaskable.ci-runner=my-pc"])
    );
    // Never a prune that reaches all containers: both filters are present.
    assert_eq!(a.iter().filter(|s| *s == "--filter").count(), 2);
}

#[test]
fn the_image_prune_is_dangling_only_and_a_day_old_never_all() {
    let a = image_prune_args();
    assert_eq!(a, ["image", "prune", "-f", "--filter", "until=24h"]);
    assert!(!a.contains(&"-a".to_string()) && !a.contains(&"--all".to_string()));
}

#[test]
fn the_build_cache_is_trimmed_over_its_cap_and_not_at_all_when_off() {
    let a = builder_prune_args(10 << 30).unwrap();
    assert_eq!(a, ["builder", "prune", "-f", "--keep-storage", "10240MB"]);
    assert_eq!(builder_prune_args(0), None);
}

#[test]
fn caps_default_and_read_the_environment() {
    let d = Caps::default();
    assert_eq!((d.build_cache, d.cache_volume, d.logs), (10 * GB, 5 * GB, 500 << 20));
    let c = Caps::from_env(&Env::from_pairs([
        ("AGENT_CI_BUILD_CACHE_GB", "3"),
        ("AGENT_CI_CACHE_GB", "0"),
        ("AGENT_CI_LOG_MB", "junk"),
    ]));
    assert_eq!((c.build_cache, c.cache_volume, c.logs), (3 * GB, 0, 500 << 20));
    assert_eq!(d.urgent().cache_volume, GB + GB / 4);
}

#[test]
fn a_listing_parses_and_skips_what_it_cannot_read() {
    let files =
        parse_listing("1700000000.5\t100\t/v/a/b\nnonsense\n1700000001.0\tx\t/v/c\n1700000002.0\t7\t/v/with space\n");
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].size, 100);
    assert_eq!(files[1].path, PathBuf::from("/v/with space"));
}

#[test]
fn the_cache_trim_deletes_the_oldest_files_until_it_fits() {
    let listing = "300\t40\t/v/new\n100\t40\t/v/oldest\n200\t40\t/v/middle\n";
    assert_eq!(plan_cache_trim(listing, 80), ["/v/oldest"]);
    assert_eq!(plan_cache_trim(listing, 40), ["/v/oldest", "/v/middle"]);
    assert!(plan_cache_trim(listing, 120).is_empty());
}

#[test]
fn the_cache_trim_never_names_a_path_outside_the_volume() {
    let listing = "1\t999\t/etc/passwd\n2\t999\t/v/../etc/shadow\n3\t999\tv/relative\n4\t10\t/v/ok\n";
    let gone = plan_cache_trim(listing, 0);
    assert_eq!(gone, ["/v/ok"]);
}

#[test]
fn the_volume_commands_mount_only_the_named_volume_and_carry_the_runners_label() {
    let list = list_args("org.retaskable.ci-runner=pc", "agent-ci-npm");
    assert!(list.contains(&"agent-ci-npm:/v:ro".to_string()) && list.contains(&"--rm".to_string()));
    let del = delete_args("org.retaskable.ci-runner=pc", "agent-ci-cargo");
    assert!(del.contains(&"agent-ci-cargo:/v".to_string()) && del.contains(&"-i".to_string()));
    assert_eq!(steps::CACHE_VOLUMES, ["agent-ci-npm", "agent-ci-cargo"]);
}

#[test]
fn a_mirror_of_a_repo_no_longer_set_is_stale_and_one_per_configured_repo_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let mirrors = dir.path().join("mirrors");
    for m in ["o-kept.git", "o-gone.git", "not-a-mirror"] {
        std::fs::create_dir_all(mirrors.join(m)).unwrap();
    }
    std::fs::write(mirrors.join("file.git"), "x").unwrap();
    let stale = stale_mirrors(&mirrors, &cfg(dir.path(), &["o/kept"]));
    assert_eq!(stale, vec![mirrors.join("o-gone.git")]);
    assert!(stale_mirrors(&dir.path().join("none"), &cfg(dir.path(), &[])).is_empty());
}

#[test]
fn logs_go_by_age_then_size_and_only_under_the_ci_logs_folders() {
    let dir = tempfile::tempdir().unwrap();
    let logs = dir.path().join("projects/o-p/ci-logs");
    std::fs::create_dir_all(&logs).unwrap();
    std::fs::write(logs.join("a.log"), vec![0u8; 100]).unwrap();
    std::fs::write(logs.join("b.log"), vec![0u8; 100]).unwrap();
    let events = dir.path().join("projects/o-p/events");
    std::fs::create_dir_all(&events).unwrap();
    std::fs::write(events.join("e.jsonl"), vec![0u8; 100]).unwrap();
    let caps = Caps {
        logs: 150,
        ..Caps::default()
    };
    let freed = prune_logs(dir.path(), &caps, false, SystemTime::now());
    assert_eq!(freed, 100, "one of the two logs fits under 150");
    assert!(events.join("e.jsonl").exists(), "events are not logs");
    // Urgent: anything over a day old goes too.
    let later = SystemTime::now() + Duration::from_secs(2 * 86400);
    prune_logs(dir.path(), &caps, true, later);
    assert!(log_dirs(dir.path()).len() == 1 && disk::list_files(&logs).is_empty());
    assert!(events.join("e.jsonl").exists());
}
