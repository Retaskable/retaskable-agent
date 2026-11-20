use super::*;

fn gb(n: f64) -> u64 {
    (n * GB as f64) as u64
}

fn at(path: &str, free_gb: f64) -> Reading {
    Reading {
        path: PathBuf::from(path),
        free: gb(free_gb),
    }
}

#[test]
fn the_defaults_are_ten_gb_and_a_five_gb_margin() {
    let l = Limits::default();
    assert_eq!((l.min_free, l.margin), (10 * GB, 5 * GB));
    let l = Limits::from_env(&Env::empty());
    assert_eq!(l, Limits::default());
}

#[test]
fn the_environment_sets_the_threshold_and_a_bad_value_is_ignored() {
    let l = Limits::from_env(&Env::from_pairs([
        ("RETASKABLE_DISK_MIN_FREE_GB", "20"),
        ("RETASKABLE_DISK_RESUME_MARGIN_GB", "lots"),
    ]));
    assert_eq!((l.min_free, l.margin), (20 * GB, 5 * GB));
    assert!(Limits::from_env(&Env::from_pairs([("RETASKABLE_DISK_MIN_FREE_GB", "0")])).off());
}

#[test]
fn it_holds_below_the_threshold_and_not_at_it() {
    let mut g = Guard::new(Limits::default());
    assert_eq!(g.observe(&[at("/data", 10.0)]), Change::Same);
    assert!(!g.holding());
    assert_eq!(g.observe(&[at("/data", 9.9)]), Change::Held);
    assert!(g.holding());
}

#[test]
fn it_resumes_only_above_the_threshold_plus_the_margin() {
    let mut g = Guard::new(Limits::default());
    g.observe(&[at("/data", 2.0)]);
    // Back over the threshold but inside the margin: still holding.
    assert_eq!(g.observe(&[at("/data", 12.0)]), Change::Same);
    assert!(g.holding());
    assert_eq!(g.observe(&[at("/data", 14.9)]), Change::Same);
    assert!(g.holding());
    assert_eq!(g.observe(&[at("/data", 15.0)]), Change::Resumed);
    assert!(!g.holding());
    // And it does not hold again until below the threshold itself.
    assert_eq!(g.observe(&[at("/data", 11.0)]), Change::Same);
}

#[test]
fn the_lowest_folder_decides_and_every_folder_must_clear_to_resume() {
    let mut g = Guard::new(Limits::default());
    assert_eq!(g.observe(&[at("/data", 80.0), at("/work", 3.0)]), Change::Held);
    assert!(g.problem().unwrap().contains("/work"));
    assert_eq!(g.observe(&[at("/data", 80.0), at("/work", 14.0)]), Change::Same);
    assert_eq!(g.observe(&[at("/data", 80.0), at("/work", 40.0)]), Change::Resumed);
}

#[test]
fn no_readings_change_nothing_and_a_zero_threshold_never_holds() {
    let mut g = Guard::new(Limits::default());
    g.observe(&[at("/d", 1.0)]);
    assert_eq!(g.observe(&[]), Change::Same);
    assert!(g.holding());
    let mut off = Guard::new(Limits { min_free: 0, margin: 0 });
    assert_eq!(off.observe(&[at("/d", 0.0)]), Change::Same);
    assert!(!off.holding());
}

#[test]
fn the_problem_names_the_free_space_and_the_path() {
    let mut g = Guard::new(Limits::default());
    assert_eq!(g.problem(), None);
    g.observe(&[at("/data/ci", 3.04)]);
    assert_eq!(
        g.problem().unwrap(),
        "Disk nearly full: 3.0 GB free on /data/ci; cleaning up, taking no new work"
    );
    assert!(g.problem().unwrap().chars().count() <= 200, "fits `refused`");
}

#[test]
fn free_bytes_reads_a_real_folder_and_a_missing_one_under_it() {
    let dir = tempfile::tempdir().unwrap();
    assert!(free_bytes(dir.path()).is_some_and(|b| b > 0));
    assert!(free_bytes(&dir.path().join("not").join("yet")).is_some());
}

fn file(name: &str, size: u64, age_days: u64, now: SystemTime) -> FileInfo {
    FileInfo {
        path: PathBuf::from(name),
        size,
        modified: now - Duration::from_secs(age_days * 86400),
    }
}

#[test]
fn old_files_go_by_age() {
    let now = SystemTime::now();
    let files = [file("old", 1, 40, now), file("new", 1, 1, now)];
    let gone = plan_prune(&files, now, Some(Duration::from_secs(30 * 86400)), None);
    assert_eq!(gone, vec![PathBuf::from("old")]);
}

#[test]
fn over_the_size_cap_the_oldest_go_first_until_it_fits() {
    let now = SystemTime::now();
    let files = [
        file("a", 40, 1, now),
        file("b", 40, 5, now),
        file("c", 40, 3, now),
        file("d", 40, 9, now),
    ];
    let gone = plan_prune(&files, now, None, Some(90));
    assert_eq!(gone, vec![PathBuf::from("d"), PathBuf::from("b")]);
    assert!(plan_prune(&files, now, None, Some(160)).is_empty());
}

#[test]
fn age_goes_first_and_the_cap_counts_only_what_is_left() {
    let now = SystemTime::now();
    let files = [
        file("ancient", 100, 90, now),
        file("a", 50, 2, now),
        file("b", 50, 1, now),
    ];
    let gone = plan_prune(&files, now, Some(Duration::from_secs(30 * 86400)), Some(100));
    assert_eq!(gone, vec![PathBuf::from("ancient")]);
}

#[test]
fn prune_files_deletes_only_regular_files_under_the_given_folders() {
    let root = tempfile::tempdir().unwrap();
    let mine = root.path().join("mine");
    let other = root.path().join("other");
    std::fs::create_dir_all(mine.join("sub")).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(mine.join("sub").join("a.log"), vec![0u8; 100]).unwrap();
    std::fs::write(mine.join("b.log"), vec![0u8; 100]).unwrap();
    std::fs::write(other.join("keep.log"), vec![0u8; 100]).unwrap();
    // Everything is "old" from a point far ahead.
    let future = SystemTime::now() + Duration::from_secs(90 * 86400);
    let freed = prune_files(
        std::slice::from_ref(&mine),
        future,
        Some(Duration::from_secs(86400)),
        None,
    );
    assert_eq!(freed, 200);
    assert!(!mine.join("b.log").exists());
    assert!(mine.join("sub").is_dir(), "folders stay");
    assert!(
        other.join("keep.log").exists(),
        "outside the given folders nothing is touched"
    );
}

#[test]
fn the_held_handle_carries_the_problem() {
    let h = Held(Mutex::new(None));
    assert_eq!(h.problem(), None);
    h.set(Some("x".into()));
    assert_eq!(h.problem().as_deref(), Some("x"));
    h.set(None);
    assert_eq!(h.problem(), None);
}
