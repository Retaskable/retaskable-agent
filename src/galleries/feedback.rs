//! Rendering a gallery feedback request (06.8's **Feedback** bullet, agent/src/galleries.ts's
//! `feedback`): the text of `prompts/gallery-feedback.md` for the spec drafter. Pure: it reads
//! the gallery's `index.json` and nothing else.
use std::path::Path;

use super::{gallery_index, valid_picture_file};
use crate::config::{GalleryConfig, ProjectType};
use crate::paths;
use crate::prompt::{project_words, render_prompt};

const TEMPLATE: &str = include_str!("../../prompts/gallery-feedback.md");

/// The shared-backlog wording of `{{galleryEntry}}`: the drafter returns the entry's title and
/// `itemId` (06.5) and the spec carries no Items header.
fn gallery_entry(gallery: &str) -> String {
    format!(
        "**The entry goes under a \"{gallery}\" theme.** Use that as the spec's `Theme:` line, and return the entry's title and a short readable kebab-case `itemId` (2 to 4 lowercase words). It depends on nothing unless the fix really needs something that isn't built."
    )
}

/// Renders the request for `notes` about the gallery called `gallery_slug` (or, with
/// `picture_file`, one of its pictures). Refusals are the commands' nack reasons.
pub fn feedback_request(
    data_dir: &Path,
    project: &str,
    galleries: &[GalleryConfig],
    project_type: ProjectType,
    gallery_slug: &str,
    picture_file: Option<&str>,
    notes: &str,
) -> Result<String, String> {
    let Some(gallery) = galleries.iter().find(|g| g.slug() == gallery_slug) else {
        return Err(format!("No gallery called {gallery_slug}."));
    };
    let dir = paths::gallery_dir(data_dir, project, gallery_slug);
    if !dir.join("index.json").is_file() {
        return Err(format!("No pictures in {} yet: shoot it first.", gallery.name));
    }
    let notes = notes.trim();
    if notes.is_empty() {
        return Err("Say what should change.".to_string());
    }
    let picture = match picture_file {
        None => "It's about the gallery as a whole.".to_string(),
        Some(file) => {
            let no_picture = || format!("No picture {file} in {}", gallery.name);
            if !valid_picture_file(file) {
                return Err(no_picture());
            }
            let index = gallery_index(data_dir, project, gallery_slug)?;
            let image = index.images.iter().find(|i| i.file == file).ok_or_else(no_picture)?;
            let group = image.group.as_deref().filter(|g| !g.is_empty());
            format!(
                "It's about the picture **{}**{}, saved at `{}`. Look at it with your Read tool before writing.",
                image.title,
                group.map(|g| format!(" ({g})")).unwrap_or_default(),
                dir.join(&image.file).display()
            )
        }
    };
    let words = project_words(project_type);
    render_prompt(
        TEMPLATE,
        &[
            ("projectNoun", words.noun.to_string()),
            ("gallery", gallery.name.clone()),
            ("galleryEntry", gallery_entry(&gallery.name)),
            ("picture", picture),
            ("notes", notes.to_string()),
        ],
    )
    .map_err(|e| e.to_string())
}
