//! The ideas prompt's `context` and user types, built from the store (Retaskable.com's
//! `agent/src/ideas.ts` `context()` and `write()`'s `profiles`).
use std::path::Path;

use anyhow::{Context as _, Result};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::process::Command;

use super::profiles::{Profile, starter_profiles};
use super::store::{IdeaState, StoreIdeas};
use crate::backlog::{Item, ItemStatus, asks::Audience};
use crate::text::clip;

/// How many of the most recently live titles the prompt names.
pub const LIVE_MAX: usize = 12;

/// `{ live, accepted, later, never, waiting, people }` (`context()`, agent/src/ideas.ts): `live` is
/// the titles of what went live (newest first, at most [`LIVE_MAX`]); the rest are the store rows'
/// titles by state, with Claude's waiting ideas under `waiting` and a person's (`addedBy` set) under
/// `people`.
pub fn round_context(live: &[String], ideas: &StoreIdeas) -> Value {
    let titles = |state: IdeaState, by_person: bool| -> Vec<&str> {
        ideas
            .ideas
            .iter()
            .filter(|i| i.state == state && (state != IdeaState::Waiting || i.added_by.is_some() == by_person))
            .map(|i| i.title.as_str())
            .collect()
    };
    json!({
        "live": live.iter().take(LIVE_MAX).collect::<Vec<_>>(),
        "accepted": titles(IdeaState::Accepted, false),
        "later": titles(IdeaState::Later, false),
        "never": titles(IdeaState::Never, false),
        "waiting": titles(IdeaState::Waiting, false),
        "people": titles(IdeaState::Waiting, true),
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MergedAt {
    url: String,
    merged_at: String,
}

/// Titles of the done items (`on_main`, `in_production`), newest merge first by their PR's merge
/// time from `gh pr list --state merged --base <base_branch>`; an item whose PR isn't in that list
/// goes last. At most [`LIVE_MAX`].
pub async fn gather_live(
    gh_bin: &Path,
    cwd: &Path,
    repo: &str,
    base_branch: &str,
    items: &[Item],
) -> Result<Vec<String>> {
    let output = Command::new(gh_bin)
        .args(["pr", "list", "--repo", repo, "--state", "merged", "--base", base_branch])
        .args(["--limit", "200", "--json", "url,mergedAt"])
        .current_dir(cwd)
        .output()
        .await
        .context("running gh pr list")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        anyhow::bail!("gh pr list failed: {}", clip(&stderr, 300));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let prs: Vec<MergedAt> = if stdout.trim().is_empty() {
        Vec::new()
    } else {
        serde_json::from_str(&stdout).context("parsing gh pr list output")?
    };
    let mut done: Vec<(Option<&str>, &str)> = items
        .iter()
        .filter(|i| matches!(i.status, ItemStatus::OnMain | ItemStatus::InProduction))
        .map(|i| {
            let at = i
                .pr_url
                .as_deref()
                .and_then(|u| prs.iter().find(|p| p.url == u))
                .map(|p| p.merged_at.as_str());
            (at, i.title.as_str())
        })
        .collect();
    // Newest first; `None` sorts before `Some`, so the reversed order puts unmatched items last.
    done.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(done.into_iter().take(LIVE_MAX).map(|(_, t)| t.to_string()).collect())
}

/// The user types for the prompt: the store's audiences (`short` as the one-line `who`), or the
/// starter set when the store has none (`audiencesOf`, shared/audiences.ts).
pub fn profiles_for(audiences: &[Audience]) -> Vec<Profile> {
    if audiences.is_empty() {
        return starter_profiles();
    }
    audiences
        .iter()
        .map(|a| Profile {
            id: a.id.clone(),
            name: a.name.clone(),
            who: a.short.clone().unwrap_or_default(),
            part: String::new(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ideas::store::StoreIdea;

    fn row(title: &str, state: IdeaState, person: bool) -> StoreIdea {
        StoreIdea {
            id: title.to_string(),
            title: title.to_string(),
            state,
            idea: None,
            at: 0,
            answer: None,
            added_by: person.then(|| json!({ "login": "sam" })),
        }
    }

    #[test]
    fn round_context_sorts_titles_by_state_and_author() {
        let ideas = StoreIdeas {
            round: None,
            ideas: vec![
                row("A", IdeaState::Accepted, false),
                row("L", IdeaState::Later, false),
                row("N", IdeaState::Never, false),
                row("W", IdeaState::Waiting, false),
                row("P", IdeaState::Waiting, true),
            ],
        };
        let live: Vec<String> = (0..15).map(|n| format!("t{n}")).collect();
        let c = round_context(&live, &ideas);
        assert_eq!(c["accepted"], json!(["A"]));
        assert_eq!(c["later"], json!(["L"]));
        assert_eq!(c["never"], json!(["N"]));
        assert_eq!(c["waiting"], json!(["W"]));
        assert_eq!(c["people"], json!(["P"]));
        assert_eq!(c["live"].as_array().unwrap().len(), 12);
        assert_eq!(c["live"][0], "t0");
    }

    fn item(title: &str, status: ItemStatus, pr: Option<&str>) -> Item {
        Item {
            id: title.to_string(),
            title: title.to_string(),
            status,
            pr_url: pr.map(ToString::to_string),
            ..Item::default()
        }
    }

    #[tokio::test]
    async fn gather_live_orders_done_items_by_merge_time_with_unmatched_last() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("prs.json");
        std::fs::write(
            &out,
            r#"[{"url":"u1","mergedAt":"2026-09-01T00:00:00Z"},{"url":"u2","mergedAt":"2026-10-01T00:00:00Z"}]"#,
        )
        .unwrap();
        let gh = dir
            .path()
            .join(if cfg!(windows) { "fake_gh.cmd" } else { "fake_gh.sh" });
        if cfg!(windows) {
            std::fs::write(&gh, format!("@type \"{}\"\r\n", out.display())).unwrap();
        } else {
            crate::test_support::write_executable(&gh, &format!("#!/bin/sh\ncat \"{}\"\n", out.display()));
        }
        let items = vec![
            item("Old", ItemStatus::OnMain, Some("u1")),
            item("Loose", ItemStatus::InProduction, Some("nope")),
            item("Busy", ItemStatus::Building, Some("u2")),
            item("New", ItemStatus::InProduction, Some("u2")),
        ];
        let live = gather_live(&gh, dir.path(), "x/y", "main", &items).await.unwrap();
        assert_eq!(live, ["New", "Old", "Loose"]);
    }

    #[test]
    fn profiles_for_maps_audiences_and_falls_back_to_starters() {
        let a = Audience {
            id: "dev".into(),
            name: "Developer".into(),
            short: Some("Writes code.".into()),
        };
        let p = profiles_for(&[a]);
        assert_eq!(
            (p[0].id.as_str(), p[0].name.as_str(), p[0].who.as_str()),
            ("dev", "Developer", "Writes code.")
        );
        assert_eq!(profiles_for(&[]).len(), 8);
    }
}
