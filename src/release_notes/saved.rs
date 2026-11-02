//! The saved release notes on the shared backlog (Retaskable.com's last-local-state.md section 3.5,
//! `agent/src/storeReleases.ts`): the store keeps them, one `releases` row a note, for every computer
//! and the site. Before the first read this computer offers the store, once, the notes it finds in the
//! repo's `docs/releases` folder on the base branch (`releases_offer`, remembered in
//! `releases.offered.json`); a note the store already has is left as it is. The files stay in the repo:
//! this computer never writes to the repo without a change someone approves. Saving a note
//! (`release_set`) is the site's, as a writer: this computer only reads them (`releases_get`).
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::process::Command;

use crate::backlog::{BacklogError, Client};
use crate::fs_util::write_json_atomic;

/// The fixed folder an older repo's saved notes are moved over from (release-notes.md section 9).
pub const RELEASES_DIR: &str = "docs/releases";
/// The most note files read from it: the last by name.
const MAX_SAVED: usize = 200;
/// The largest note the store takes, as UTF-8 (`shared/storeReleases.ts`).
pub const RELEASE_BODY_MAX_BYTES: usize = 128 * 1024;
/// The longest title the store takes, in characters.
pub const RELEASE_TITLE_MAX: usize = 300;

/// A note's id: a file-name-like word, so a note moved over from `docs/releases` keeps its file's name.
static RELEASE_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9][\w.-]{0,79}$").unwrap());
static FRONT_MATTER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^---\r?\n((?s:.*?))\r?\n---").unwrap());
static FIRST_HEADING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^#\s+(.+?)\s*$").unwrap());

/// The file `releases_offer` is remembered in, under the project's own folder.
pub fn offered_file(project_dir: &Path) -> PathBuf {
    project_dir.join("releases.offered.json")
}

/// One saved note as the store answers it: `releases_get` lists them without `body`, newest first;
/// with an `id` it answers that one with its Markdown.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreRelease {
    pub id: String,
    pub title: String,
    /// The range's days (`2026-09-23`), as the note's front matter says.
    pub from: String,
    pub to: String,
    /// The Markdown, front matter included; only when one note is asked for.
    #[serde(default)]
    pub body: Option<String>,
    /// When it was saved, in ms.
    #[serde(default)]
    pub created_at: u64,
    /// The login whose write it was.
    #[serde(default)]
    pub by: String,
    /// The computer that wrote it, when an agent did.
    #[serde(default)]
    pub machine: Option<String>,
}

/// A note file in the repo's `docs/releases` folder.
#[derive(Debug, Clone)]
pub struct ReleaseFile {
    pub path: String,
    pub content: String,
}

/// What `releases_offer` sends for one file.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseOffer {
    pub id: String,
    pub title: String,
    pub from: String,
    pub to: String,
    pub body: String,
    pub created_at: i64,
}

/// A front matter day (`from: 2026-09-23`, quoted or not).
fn front_day(front: &str, key: &str) -> Option<String> {
    let re = Regex::new(&format!(r#"(?m)^{key}:\s*["']?(\d{{4}}-\d{{2}}-\d{{2}})"#)).unwrap();
    re.captures(front).map(|c| c[1].to_string())
}

/// A note file as the store's row (`releaseOfFile`), or `None` when it can't be one: no `from`/`to`
/// days in its front matter, a name the store won't take as an id, or an empty or too-long body.
/// Its title is its first `#` heading, else its id; it was saved at the start of its `to` day.
pub fn release_of_file(file: &ReleaseFile) -> Option<ReleaseOffer> {
    let front = FRONT_MATTER
        .captures(&file.content)
        .map(|c| c[1].to_string())
        .unwrap_or_default();
    let from = front_day(&front, "from")?;
    let to = front_day(&front, "to")?;
    let name = file.path.rsplit('/').next().unwrap_or(&file.path);
    let id = name.strip_suffix(".md").unwrap_or(name);
    if !RELEASE_ID.is_match(id) || file.content.trim().is_empty() || file.content.len() > RELEASE_BODY_MAX_BYTES {
        return None;
    }
    let title = FIRST_HEADING
        .captures(&file.content)
        .map(|c| c[1].to_string())
        .unwrap_or_else(|| id.to_string());
    let created_at = chrono::NaiveDate::parse_from_str(&to, "%Y-%m-%d")
        .ok()?
        .and_hms_opt(0, 0, 0)?
        .and_utc()
        .timestamp_millis();
    Some(ReleaseOffer {
        id: id.to_string(),
        title: title.chars().take(RELEASE_TITLE_MAX).collect(),
        from,
        to,
        body: file.content.clone(),
        created_at,
    })
}

/// The note files in `docs/releases` at `base_ref` (`readReleaseFiles`), the last `MAX_SAVED` by name.
/// None when the folder isn't there; a file that can't be read comes back empty, which
/// [`release_of_file`] then leaves out.
pub async fn read_release_files(git_bin: &Path, cwd: &Path, base_ref: &str) -> Vec<ReleaseFile> {
    let git = |args: Vec<String>| {
        let mut cmd = Command::new(git_bin);
        cmd.args(args).current_dir(cwd);
        cmd
    };
    let ls = git(vec![
        "ls-tree".into(),
        "-z".into(),
        "--name-only".into(),
        format!("{base_ref}:{RELEASES_DIR}"),
    ])
    .output()
    .await;
    let ls = match ls {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(), // no folder yet
    };
    let mut names: Vec<String> = String::from_utf8_lossy(&ls.stdout)
        .split('\0')
        .filter(|n| n.to_lowercase().ends_with(".md"))
        .map(str::to_string)
        .collect();
    names.sort();
    let skip = names.len().saturating_sub(MAX_SAVED);
    let mut files = Vec::new();
    for name in names.into_iter().skip(skip) {
        let path = format!("{RELEASES_DIR}/{name}");
        let shown = git(vec!["show".into(), format!("{base_ref}:{path}")]).output().await;
        let content = match shown {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
            _ => String::new(),
        };
        files.push(ReleaseFile { path, content });
    }
    files
}

#[derive(Deserialize)]
struct Offered {
    #[serde(default)]
    done: bool,
}

#[derive(Deserialize)]
struct Taken {
    #[serde(default)]
    taken: Vec<String>,
}

/// One project's saved notes in the store (`StoreReleases`): the offer once, then `releases_get`.
pub struct SavedReleases<'a> {
    pub client: &'a Client,
    pub repo_id: u64,
    pub token: Option<&'a str>,
    /// [`offered_file`] for the project.
    pub offered_file: PathBuf,
    pub machine: String,
}

impl SavedReleases<'_> {
    fn offered(&self) -> bool {
        std::fs::read_to_string(&self.offered_file)
            .ok()
            .and_then(|s| serde_json::from_str::<Offered>(&s).ok())
            .is_some_and(|o| o.done)
    }

    /// Offers the repo's note files once (`files` reads them, and isn't called once they've been offered).
    /// False when the store couldn't be asked, so the next read tries again. A store that refuses them
    /// (over its cap or out of shape) isn't asked again: sending them again wouldn't help, and they stay
    /// in the repo only.
    async fn ready(&self, files: impl AsyncFnOnce() -> Vec<ReleaseFile>) -> bool {
        if self.offered() {
            return true;
        }
        let releases: Vec<ReleaseOffer> = files().await.iter().filter_map(release_of_file).collect();
        if !releases.is_empty() {
            let args = json!({ "machine": self.machine, "releases": releases });
            match self
                .client
                .op::<Taken>(self.repo_id, self.token, "releases_offer", args, "offer")
                .await
            {
                Ok(Taken { taken }) if !taken.is_empty() => tracing::info!(
                    "releases: offered this repo's saved release notes to the shared backlog, which took {}",
                    taken.len()
                ),
                Ok(_) => {}
                Err(e) if e.code() == Some("bad_request") => tracing::warn!(
                    "releases: the shared backlog refused the repo's saved release notes, so they stay in the repo only: {e}"
                ),
                Err(e) => {
                    tracing::warn!(
                        "releases: couldn't offer the repo's saved release notes to the shared backlog, trying again at the next read: {e}"
                    );
                    return false;
                }
            }
        }
        if let Err(e) = write_json_atomic(&self.offered_file, &json!({ "done": true }), false) {
            tracing::warn!("releases: couldn't remember the offer, so it's made again next time: {e:#}");
        }
        true
    }

    /// The saved notes without their Markdown, newest first, or `None` when the store can't give them
    /// right now (never a guessed or empty list: the store is the only place they're kept).
    pub async fn list(&self, files: impl AsyncFnOnce() -> Vec<ReleaseFile>) -> Option<Vec<StoreRelease>> {
        if !self.ready(files).await {
            return None;
        }
        match self
            .client
            .op(self.repo_id, self.token, "releases_get", json!({}), "saved notes")
            .await
        {
            Ok(rows) => Some(rows),
            Err(e) => {
                tracing::warn!("releases: couldn't read the saved release notes from the shared backlog: {e}");
                None
            }
        }
    }

    /// One saved note with its Markdown. A note the store hasn't got is its `not_found` refusal.
    pub async fn get(&self, id: &str) -> Result<StoreRelease, BacklogError> {
        self.client
            .op(
                self.repo_id,
                self.token,
                "releases_get",
                json!({ "id": id }),
                "saved note",
            )
            .await
    }
}

#[cfg(test)]
#[path = "saved_tests.rs"]
mod tests;
