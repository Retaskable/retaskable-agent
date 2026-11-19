//! The CI runner's repo ids, kept in `<ci data dir>/repo_ids.json` (`{ "owner/name": 123 }`) so a restart doesn't need
//! GitHub, whose API limit may be used up. A repo's id never changes for a given `owner/name`, so an entry holds for as
//! long as the repo keeps its name; a renamed repo has no entry and is asked for. The runner has no project state of its
//! own (the agent's `state.json` is the agent's), and it is this file's only writer.
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const FILE: &str = "repo_ids.json";

pub fn file(dir: &Path) -> PathBuf {
    dir.join(FILE)
}

/// What the file holds; empty when it is missing or unreadable.
pub fn load(file: &Path) -> HashMap<String, u64> {
    std::fs::read_to_string(file)
        .ok()
        .and_then(|t| serde_json::from_str::<HashMap<String, u64>>(&t).ok())
        .map(|m| m.into_iter().filter(|(_, id)| *id > 0).collect())
        .unwrap_or_default()
}

/// Saves `ids`; a failure is said and goes on (the id is still known in memory).
pub fn save(file: &Path, ids: &HashMap<String, u64>) {
    let value = serde_json::json!(ids);
    if let Err(e) = crate::fs_util::write_json_atomic(file, &value, true) {
        tracing::warn!("couldn't save the repo ids to {}: {e:#}", file.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_ci::store::StoreOps;
    use crate::link::FileStore;
    use std::sync::Arc;

    fn ops(dir: &Path, gh: &Path) -> StoreOps {
        let keys = Arc::new(FileStore::new(dir.join("keys.json"), "test"));
        StoreOps::new("http://127.0.0.1:1".into(), keys)
            .keeping(dir)
            .with_gh(gh)
    }

    fn fake_gh(dir: &Path, body: &str) -> PathBuf {
        let p = dir.join("gh");
        crate::test_support::write_executable(&p, &format!("#!/bin/sh\n{body}\n"));
        p
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn a_restart_asks_gh_only_for_a_repo_it_has_no_id_for() {
        let dir = tempfile::tempdir().unwrap();
        let calls = dir.path().join("calls");
        let gh = fake_gh(dir.path(), &format!("echo x >> {}\necho 77", calls.display()));
        let probed = std::fs::read_to_string(&calls).map_or(0, |t| t.lines().count());
        let asked = || std::fs::read_to_string(&calls).map_or(0, |t| t.lines().count()) - probed;

        assert_eq!(ops(dir.path(), &gh).repo_id("o/r").await, Ok(77));
        assert_eq!(asked(), 1);
        // Restarted: the saved id is used, with no call to gh.
        assert_eq!(ops(dir.path(), &gh).repo_id("o/r").await, Ok(77));
        assert_eq!(asked(), 1);
        // Another name asks again.
        assert_eq!(ops(dir.path(), &gh).repo_id("o/renamed").await, Ok(77));
        assert_eq!(asked(), 2);

        // GitHub's limit used up: the saved id still serves, and a new repo says why not.
        let limited = fake_gh(dir.path(), "echo 'API rate limit exceeded (HTTP 403)' >&2; exit 1");
        assert_eq!(ops(dir.path(), &limited).repo_id("o/r").await, Ok(77));
        assert!(ops(dir.path(), &limited).repo_id("o/new").await.is_err());
    }
}
