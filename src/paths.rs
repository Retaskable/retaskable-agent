//! Where a project's own files live under the data directory, following `agent/src/paths.ts` in
//! Retaskable.com (`projectDir`, `runDir`).
use std::path::{Path, PathBuf};

/// A project's own folder under the data directory: its state, transcripts and gallery pictures.
pub fn project_dir(data_dir: &Path, project: &str) -> PathBuf {
    data_dir.join("projects").join(project)
}

/// Where one item's session transcripts live (`runDir`).
pub fn run_dir(data_dir: &Path, project: &str, item_id: &str) -> PathBuf {
    project_dir(data_dir, project).join("runs").join(item_id)
}

/// Where a gallery's converted pictures and `index.json` live (`galleryDir`, spec section 6.8).
pub fn gallery_dir(data_dir: &Path, project: &str, slug: &str) -> PathBuf {
    project_dir(data_dir, project).join("galleries").join(slug)
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
    fn gallery_dir_nests_under_the_project_and_galleries() {
        let data_dir = Path::new("/data");
        assert_eq!(
            gallery_dir(data_dir, "my-project", "launch-shoot"),
            Path::new("/data/projects/my-project/galleries/launch-shoot")
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
