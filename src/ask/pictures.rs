//! The pictures a chat message carries (`args.pictureIds` of an `ask` row, docs/specs/intent-rows.md section 3.2):
//! fetched from the store into a folder the session can read, listed for it in an "Attached images" block, and
//! removed when the question is done. The id is checked before it is used as a file name.
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::backlog::{BacklogError, Client};

/// The folder pictures are saved in, under the computer's temp folder.
pub fn folder() -> PathBuf {
    std::env::temp_dir().join("retaskable-pictures")
}

/// The ids in a row's `pictureIds`, checked: a 36-character id and one of the four picture extensions. Anything else
/// (not a list, not text, a path) is dropped.
pub fn ids_of(args: &Value) -> Vec<String> {
    let Some(list) = args.get("pictureIds").and_then(Value::as_array) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(Value::as_str)
        .filter(|id| valid(id))
        .map(String::from)
        .collect()
}

fn valid(id: &str) -> bool {
    let Some((stem, ext)) = id.rsplit_once('.') else {
        return false;
    };
    stem.len() == 36
        && stem.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f' | '-'))
        && matches!(ext, "png" | "jpg" | "gif" | "webp")
}

/// Pictures saved for one question; they are deleted when it is dropped.
#[derive(Debug, Default)]
pub struct Pictures {
    paths: Vec<PathBuf>,
}

impl Pictures {
    /// The "Attached images" block for the prompt, empty when there are none.
    pub fn block(&self) -> String {
        if self.paths.is_empty() {
            return String::new();
        }
        let list: Vec<String> = self.paths.iter().map(|p| format!("- {}", p.display())).collect();
        format!(
            "Attached images (look at each with your Read tool before acting on this):\n{}",
            list.join("\n")
        )
    }

    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }
}

impl Drop for Pictures {
    fn drop(&mut self) {
        for path in &self.paths {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Fetches each picture from the store into `dir`. A picture the store won't give, or that can't be saved, fails the
/// lot with a plain reason (and what was saved is removed): answering a question about a picture without it would
/// mislead.
pub async fn fetch(
    client: &Client,
    repo: u64,
    token: Option<&str>,
    ids: &[String],
    dir: &Path,
) -> Result<Pictures, String> {
    let mut saved = Pictures::default();
    if ids.is_empty() {
        return Ok(saved);
    }
    std::fs::create_dir_all(dir).map_err(|_| "Couldn't save the attached pictures.".to_string())?;
    for id in ids {
        let bytes = client.picture(repo, token, id).await.map_err(|e: BacklogError| {
            tracing::warn!("picture {id}: {e}");
            "Couldn't read the attached pictures.".to_string()
        })?;
        let path = dir.join(id);
        std::fs::write(&path, bytes).map_err(|_| "Couldn't save the attached pictures.".to_string())?;
        saved.paths.push(path);
    }
    Ok(saved)
}

#[cfg(test)]
mod tests;
