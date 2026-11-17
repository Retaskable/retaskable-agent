//! The last release (release-notes.md section 3.3; `pickLast` and `lastRelease` in the TS agent's `releaseNotes.ts`):
//! the newest saved note, else the newest release on the code host, else the newest tag. Each source is only asked
//! when the one before it found nothing.
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::release_notes::{SavedReleases, StoreRelease};

/// How long a last release kept as a fact is trusted before it is looked up again (intent-rows.md section 5.2).
pub const EVERY: Duration = Duration::from_secs(5 * 60);

const QUICK: Duration = Duration::from_secs(60);

/// The last release: a day (`2026-09-12`, a saved note's `to`) or an ISO instant, and what it was.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Last {
    pub at: String,
    pub label: String,
}

/// A release on the code host, as `gh release list --json publishedAt,tagName` says it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostRelease {
    #[serde(default)]
    pub published_at: String,
    #[serde(default)]
    pub tag_name: String,
}

/// The last release from what each source said (`saved` is newest first). Pure.
pub fn pick_last(saved: &[StoreRelease], release: Option<&HostRelease>, tag: Option<(&str, &str)>) -> Option<Last> {
    if let Some(note) = saved.first() {
        return Some(Last {
            at: note.to.clone(),
            label: note.id.strip_suffix(".md").unwrap_or(&note.id).to_string(),
        });
    }
    if let Some(r) = release.filter(|r| !r.published_at.is_empty()) {
        return Some(Last {
            at: r.published_at.clone(),
            label: r.tag_name.clone(),
        });
    }
    tag.filter(|(_, date)| !date.is_empty()).map(|(name, date)| Last {
        at: date.to_string(),
        label: name.to_string(),
    })
}

/// The newest published release on the host (`gh release list`), none when there is none or `gh` can't say.
async fn host_release(gh: &Path, dir: &Path, repo: &str) -> Option<HostRelease> {
    let args = [
        "release",
        "list",
        "--repo",
        repo,
        "--limit",
        "1",
        "--exclude-drafts",
        "--json",
        "publishedAt,tagName",
    ];
    let out = crate::promote::host::gh(gh, dir, &args, QUICK).await.ok()?;
    serde_json::from_str::<Vec<HostRelease>>(&out).ok()?.into_iter().next()
}

/// The newest tag reachable from `origin/<base>` and its commit's date.
async fn newest_tag(dir: &Path, base: &str) -> Option<(String, String)> {
    use crate::promote::git::run;
    let at = format!("origin/{base}");
    let described = run(dir, &["describe", "--tags", "--abbrev=0", &at], QUICK).await.ok()?;
    let name = described.stdout.trim().to_string();
    if !described.ok || name.is_empty() {
        return None;
    }
    let commit = format!("{name}^{{commit}}");
    let date = run(dir, &["log", "-1", "--format=%cI", &commit], QUICK).await.ok()?;
    Some((
        name,
        if date.ok {
            date.stdout.trim().to_string()
        } else {
            String::new()
        },
    ))
}

/// Looks the last release up. The outer `None` is the store's saved notes not being reachable, which keeps what was
/// there (never a guess); the inner is no release at all.
pub async fn find(saved: &SavedReleases<'_>, gh: &Path, dir: &Path, repo: &str, base: &str) -> Option<Option<Last>> {
    let notes = saved.list().await?;
    if !notes.is_empty() {
        return Some(pick_last(&notes, None, None));
    }
    // Without a repository name there is no host to ask (and `gh` would only answer with an error).
    if !repo.is_empty()
        && let Some(release) = host_release(gh, dir, repo).await
    {
        return Some(pick_last(&[], Some(&release), None));
    }
    let tag = newest_tag(dir, base).await;
    Some(pick_last(
        &[],
        None,
        tag.as_ref().map(|(n, d)| (n.as_str(), d.as_str())),
    ))
}
