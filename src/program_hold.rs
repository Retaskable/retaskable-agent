//! Items held in checking because a check's program isn't installed on this computer (`CheckRun::missing_program`).
//! While held, the checks are not run again every loop: only whether the missing programs are on `PATH` now is looked
//! up, at most once per [`RECHECK`]; the full checks run again once they are all found. Kept in memory, and saved with
//! the project's state (`snapshot`, `restore`): after a restart the item goes straight back to the PATH lookup.
use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use crate::sync::lock;

/// How often a held item looks for its missing programs.
pub const RECHECK: Duration = Duration::from_secs(60);

struct Held {
    programs: Vec<String>,
    looked: Instant,
}

static HELD: LazyLock<Mutex<HashMap<(String, String), Held>>> = LazyLock::new(Mutex::default);

/// Holds `item` of `project` for `programs`, looked up from now. Only bare program names can be looked up on `PATH`;
/// with a path in one (`./gradlew`) nothing is held and the checks keep running as before.
pub fn hold(project: &str, item: &str, programs: Vec<String>) {
    if programs.is_empty() || programs.iter().any(|p| p.contains(['/', '\\'])) {
        return;
    }
    let held = Held {
        programs,
        looked: Instant::now(),
    };
    lock(&HELD).insert((project.to_string(), item.to_string()), held);
}

/// `project`'s held items and the programs each lacks, to save with its state.
pub fn snapshot(project: &str) -> BTreeMap<String, Vec<String>> {
    lock(&HELD)
        .iter()
        .filter(|((p, _), _)| p == project)
        .map(|((_, item), held)| (item.clone(), held.programs.clone()))
        .collect()
}

/// Takes back what `snapshot` saved, looked up afresh on the first loop. Items already held are left as they are.
pub fn restore(project: &str, saved: &BTreeMap<String, Vec<String>>) {
    let mut held = lock(&HELD);
    for (item, programs) in saved {
        if programs.is_empty() {
            continue;
        }
        let looked = Instant::now().checked_sub(RECHECK).unwrap_or_else(Instant::now);
        held.entry((project.to_string(), item.clone())).or_insert(Held {
            programs: programs.clone(),
            looked,
        });
    }
}

/// Drops the held items of `project` that `keep` doesn't name (they left checking).
pub fn retain(project: &str, keep: impl Fn(&str) -> bool) {
    lock(&HELD).retain(|(p, item), _| p != project || keep(item));
}

/// The programs a held item still lacks, while the checks should not run again: within `interval` of the last look
/// that is what it was held for; after it, the programs are looked up and those still missing are answered. `None`
/// when the item isn't held or every program is on `PATH` now (the hold ends: run the checks).
pub fn still_held(project: &str, item: &str, interval: Duration) -> Option<Vec<String>> {
    still_held_on(project, item, interval, std::env::var_os("PATH"))
}

fn still_held_on(project: &str, item: &str, interval: Duration, path: Option<OsString>) -> Option<Vec<String>> {
    let key = (project.to_string(), item.to_string());
    let mut held = lock(&HELD);
    let entry = held.get_mut(&key)?;
    if entry.looked.elapsed() < interval {
        return Some(entry.programs.clone());
    }
    let dirs: Vec<PathBuf> = path.map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    entry.programs.retain(|p| !on_path(p, &dirs));
    entry.looked = Instant::now();
    if entry.programs.is_empty() {
        held.remove(&key);
        return None;
    }
    Some(entry.programs.clone())
}

/// Whether `program` is a file in one of `dirs` (with a `PATHEXT` extension on Windows, an execute bit elsewhere).
fn on_path(program: &str, dirs: &[PathBuf]) -> bool {
    let names: Vec<String> = if cfg!(windows) {
        let exts = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
        std::iter::once(program.to_string())
            .chain(
                exts.split(';')
                    .filter(|e| !e.is_empty())
                    .map(|e| format!("{program}{e}")),
            )
            .collect()
    } else {
        vec![program.to_string()]
    };
    dirs.iter().any(|dir| {
        names.iter().any(|n| {
            std::fs::metadata(dir.join(n)).is_ok_and(|m| {
                m.is_file() && {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        m.permissions().mode() & 0o111 != 0
                    }
                    #[cfg(not(unix))]
                    {
                        true
                    }
                }
            })
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_item_only_looks_for_its_program_and_is_released_once_it_is_found() {
        let bin = tempfile::tempdir().unwrap();
        let path = std::env::join_paths([bin.path()]).unwrap();
        hold("ph-p", "ph-i", vec!["retaskable-fake-tool".into()]);
        // Within the interval: held, nothing looked up (the tool being there isn't noticed yet).
        let tool = bin.path().join(if cfg!(windows) {
            "retaskable-fake-tool.exe"
        } else {
            "retaskable-fake-tool"
        });
        std::fs::write(&tool, "").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert_eq!(
            still_held_on("ph-p", "ph-i", Duration::from_secs(3600), Some(path.clone())),
            Some(vec!["retaskable-fake-tool".to_string()])
        );
        // Due, and found: released, so the checks run.
        assert_eq!(still_held_on("ph-p", "ph-i", Duration::ZERO, Some(path)), None);
        assert_eq!(still_held("ph-p", "ph-i", RECHECK), None);
    }

    #[test]
    fn a_hold_saved_and_restored_looks_on_path_at_once_and_is_pruned_when_the_item_leaves() {
        hold("ph-p4", "ph-a", vec!["retaskable-fake-tool".into()]);
        hold("ph-p4", "ph-b", vec!["other-tool".into()]);
        let saved = snapshot("ph-p4");
        assert_eq!(saved.len(), 2);
        retain("ph-p4", |item| item == "ph-a");
        assert_eq!(snapshot("ph-p4").keys().collect::<Vec<_>>(), ["ph-a"]);
        lock(&HELD).retain(|(p, _), _| p != "ph-p4");
        restore("ph-p4", &saved);
        let empty = std::env::join_paths([tempfile::tempdir().unwrap().path()]).unwrap();
        // Due at once (not a minute after the restart), and still missing.
        assert_eq!(
            still_held_on("ph-p4", "ph-b", RECHECK, Some(empty)),
            Some(vec!["other-tool".to_string()])
        );
    }

    #[test]
    fn a_program_still_missing_when_looked_for_keeps_the_hold_and_a_path_in_it_holds_nothing() {
        let empty = std::env::join_paths([tempfile::tempdir().unwrap().path()]).unwrap();
        hold("ph-p2", "ph-i2", vec!["retaskable-fake-tool".into()]);
        assert_eq!(
            still_held_on("ph-p2", "ph-i2", Duration::ZERO, Some(empty)),
            Some(vec!["retaskable-fake-tool".to_string()])
        );
        hold("ph-p2", "ph-i3", vec!["./gradlew".into()]);
        assert_eq!(still_held("ph-p2", "ph-i3", RECHECK), None);
    }
}
