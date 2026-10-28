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
