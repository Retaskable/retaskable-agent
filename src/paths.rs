//! Where a project's own files live under the data directory, following `agent/src/paths.ts` in
//! Retaskable.com (`projectDir`, `runDir`).
use std::path::{Path, PathBuf};

/// A project's own folder under the data directory: its state and transcripts.
pub fn project_dir(data_dir: &Path, project: &str) -> PathBuf {
    data_dir.join("projects").join(project)
}

/// Where one item's session transcripts live (`runDir`).
pub fn run_dir(data_dir: &Path, project: &str, item_id: &str) -> PathBuf {
    project_dir(data_dir, project).join("runs").join(item_id)
}

/// Where a project's CI checks get their fresh directories (agent-ci.md section 3.2).
pub fn ci_dir(data_dir: &Path, project: &str) -> PathBuf {
    project_dir(data_dir, project).join("ci")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_dir_nests_under_projects() {
        let data_dir = Path::new("/data");
        assert_eq!(
            project_dir(data_dir, "my-project"),
            Path::new("/data/projects/my-project")
        );
    }

    #[test]
    fn run_dir_nests_under_the_project_and_runs() {
        let data_dir = Path::new("/data");
        assert_eq!(
            run_dir(data_dir, "my-project", "item-42"),
            Path::new("/data/projects/my-project/runs/item-42")
        );
    }

    #[test]
    fn different_projects_do_not_collide() {
        let data_dir = Path::new("/data");
        assert_ne!(
            run_dir(data_dir, "project-a", "item-1"),
            run_dir(data_dir, "project-b", "item-1")
        );
    }
}
