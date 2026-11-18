//! The project's GitHub repo id, kept in `state.json` with the `owner/name` it was found for (`repoId`:
//! `{ "repo": "owner/name", "id": 123 }`), so a restart doesn't have to ask GitHub, whose API limit may be used up.
//! A repo's id never changes for a given `owner/name`; a saved one counts only while the name still matches.
use std::path::Path;

use serde_json::json;

use super::Store;

const KEY: &str = "repoId";

impl Store {
    /// The saved repo id, when it was found for `github_repo`.
    pub fn repo_id(&self, github_repo: &str) -> Option<u64> {
        let kept = self.data.get(KEY)?;
        if kept.get("repo")?.as_str()? != github_repo {
            return None;
        }
        kept.get("id")?.as_u64().filter(|&id| id > 0)
    }

    /// Keeps `id` as `github_repo`'s, replacing whatever was kept; false when it was kept already. Doesn't save on its
    /// own; call `save` after.
    pub fn set_repo_id(&mut self, github_repo: &str, id: u64) -> bool {
        if self.repo_id(github_repo) == Some(id) {
            return false;
        }
        self.data
            .insert(KEY.to_string(), json!({ "repo": github_repo, "id": id }));
        true
    }
}

/// The repo id saved in the project's folder for `github_repo`, for a program that only reads it (the orchestrator is
/// the one that writes `state.json`).
pub fn saved_repo_id(project_dir: &Path, github_repo: &str) -> Option<u64> {
    Store::load(project_dir).ok()?.repo_id(github_repo)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kept_for_its_name_only_and_across_a_reload() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Store::load(dir.path()).unwrap();
        assert_eq!(s.repo_id("o/r"), None);
        assert!(s.set_repo_id("o/r", 42));
        assert!(!s.set_repo_id("o/r", 42));
        s.save().unwrap();
        assert_eq!(saved_repo_id(dir.path(), "o/r"), Some(42));
        assert_eq!(saved_repo_id(dir.path(), "o/renamed"), None);
        assert!(Store::load(dir.path()).unwrap().set_repo_id("o/renamed", 42));
    }
}
