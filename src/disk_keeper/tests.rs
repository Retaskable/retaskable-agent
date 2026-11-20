use super::*;

const GB: u64 = 1 << 30;

fn keeper(now: Instant) -> Keeper {
    Keeper::new(
        PathBuf::from("/data"),
        Limits::default(),
        LANE_IDLE,
        now,
        Arc::default(),
    )
}

fn free_of(gb: u64) -> impl Fn(&Path) -> Option<u64> {
    move |_| Some(gb * GB)
}

#[test]
fn a_fine_disk_gets_the_routine_cleanup_once_an_hour() {
    let t0 = Instant::now();
    let mut k = keeper(t0);
    let plan = k.tick(t0, &[], free_of(100)).unwrap();
    assert!(!plan.urgent);
    assert_eq!(plan.lane_idle, LANE_IDLE);
    // Not due again before five minutes, and the sweep not before an hour.
    assert_eq!(k.tick(t0 + Duration::from_secs(60), &[], free_of(100)), None);
    assert_eq!(k.tick(t0 + Duration::from_secs(10 * 60), &[], free_of(100)), None);
    assert!(k.tick(t0 + Duration::from_secs(3601), &[], free_of(100)).is_some());
}

#[test]
fn a_low_disk_holds_work_and_cleans_every_free_lane_at_once() {
    let t0 = Instant::now();
    let mut k = keeper(t0);
    let plan = k.tick(t0, &[], free_of(4)).unwrap();
    assert!(plan.urgent);
    assert_eq!(plan.lane_idle, Duration::ZERO);
    assert!(plan.sessions_kept < SESSIONS_KEPT);
    assert!(k.watch.guard.holding());
    assert!(
        k.held
            .problem()
            .unwrap()
            .starts_with("Disk nearly full: 4.0 GB free on /data")
    );
    // While holding it looks again every minute and cleans again.
    let again = k.tick(t0 + Duration::from_secs(61), &[], free_of(5)).unwrap();
    assert!(again.urgent);
}

#[test]
fn it_resumes_above_the_threshold_plus_the_margin_and_clears_the_problem() {
    let t0 = Instant::now();
    let mut k = keeper(t0);
    k.tick(t0, &[], free_of(4));
    // 12 GB is over 10 but inside the 5 GB margin: still holding.
    k.tick(t0 + Duration::from_secs(61), &[], free_of(12));
    assert!(k.watch.guard.holding());
    k.tick(t0 + Duration::from_secs(130), &[], free_of(16));
    assert!(!k.watch.guard.holding());
    assert_eq!(k.held.problem(), None);
}

#[test]
fn the_worktrees_folders_are_watched_too_and_the_lowest_wins() {
    let t0 = Instant::now();
    let mut k = keeper(t0);
    let work = PathBuf::from("/work/wt");
    let plan = k.tick(t0, std::slice::from_ref(&work), |p| {
        Some(if p == work { 2 * GB } else { 90 * GB })
    });
    assert!(plan.unwrap().urgent);
    assert!(k.held.problem().unwrap().contains("/work/wt"));
    k.tick(t0 + Duration::from_secs(70), std::slice::from_ref(&work), |_| {
        Some(90 * GB)
    });
}

#[test]
fn an_unreadable_disk_is_not_a_full_one() {
    let t0 = Instant::now();
    let mut k = keeper(t0);
    k.tick(t0, &[], |_| None);
    assert!(!k.watch.guard.holding());
}

#[test]
fn from_env_reads_the_lane_idle_hours() {
    let env = crate::env::Env::from_pairs([("RETASKABLE_LANE_IDLE_HOURS", "2")]);
    let k = Keeper::from_env(PathBuf::from("/d"), &env, Instant::now());
    assert_eq!(k.lane_idle, Duration::from_secs(7200));
}

#[test]
fn only_session_transcripts_are_pruned_and_by_age_and_size() {
    let data = tempfile::tempdir().unwrap();
    let runs = data.path().join("projects/p/runs/item1");
    std::fs::create_dir_all(&runs).unwrap();
    std::fs::write(runs.join("s.jsonl"), vec![0u8; 100]).unwrap();
    let state = data.path().join("projects/p/state.json");
    std::fs::write(&state, "{}").unwrap();
    let other = data.path().join("secrets.key");
    std::fs::write(&other, "k").unwrap();
    let plan = Plan {
        lane_idle: Duration::ZERO,
        sessions_kept: Duration::from_secs(86400),
        sessions_max_bytes: 1 << 30,
        urgent: true,
    };
    // Today's transcript stays.
    assert_eq!(prune_transcripts(data.path(), &plan, SystemTime::now()), 0);
    // Two days on it is past the age; nothing else is touched.
    let later = SystemTime::now() + Duration::from_secs(2 * 86400);
    assert_eq!(prune_transcripts(data.path(), &plan, later), 100);
    assert!(!runs.join("s.jsonl").exists());
    assert!(state.exists() && other.exists());
}
