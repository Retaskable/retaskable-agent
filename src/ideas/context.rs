//! The ideas round's `context` and user types, built from the store (`context()` and `write()`'s
//! `profiles`, agent/src/ideas.ts).
use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use serde::Deserialize;
use serde_json::{Value, json};

use super::profiles::{Profile, starter_profiles};
use super::store::{IdeaState, StoreIdeas};
use crate::backlog::asks::Audience;
use crate::backlog::{Item, ItemStatus};
use crate::release_notes::run_gh_list;

/// How many of the most recently live titles the prompt shows.
pub const LIVE_MAX: usize = 12;

/// `{ live, accepted, later, never, waiting, people }`: `live` is what went live (newest first, at
/// most [`LIVE_MAX`]), the next three are the titles of the store's rows in that state, `waiting` is
/// Claude's waiting ideas and `people` is people's own (`addedBy` set).
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

/// The titles of the done `items` (on main or in production), newest merge first by their PR's merge
/// time in `merged` (url -> `mergedAt`); an item whose PR isn't in it goes last. At most [`LIVE_MAX`].
pub fn live_titles(items: &[Item], merged: &HashMap<String, String>) -> Vec<String> {
    let mut done: Vec<(Option<&String>, &str)> = items
        .iter()
        .filter(|i| matches!(i.status, ItemStatus::OnMain | ItemStatus::InProduction))
        .map(|i| (i.pr_url.as_ref().and_then(|u| merged.get(u)), i.title.as_str()))
        .collect();
    // Some sorts above None; reversed so the newest is first. The sort is stable.
    done.sort_by(|a, b| b.0.cmp(&a.0));
    done.into_iter().take(LIVE_MAX).map(|(_, t)| t.to_string()).collect()
}

/// Reads `live` for a project: `gh pr list --state merged --base <base_branch> --json url,mergedAt`,
/// then [`live_titles`] over the store's `items`.
pub async fn gather_live(
    gh_bin: &Path,
    cwd: &Path,
    repo: &str,
    base_branch: &str,
    items: &[Item],
) -> Result<Vec<String>> {
    let args: Vec<String> = ["pr", "list", "--repo", repo, "--base", base_branch, "--state", "merged"]
        .into_iter()
        .chain(["--limit", "200", "--json", "url,mergedAt"])
        .map(String::from)
        .collect();
    let raw: Vec<MergedAt> = run_gh_list(gh_bin, cwd, &args).await?;
    let merged = raw.into_iter().map(|m| (m.url, m.merged_at)).collect();
    Ok(live_titles(items, &merged))
}

/// The user types for the prompt: the store's audiences (`who` falls back to their short line), or the starter
/// set when the store has none (`audiencesOf`, shared/audiences.ts).
pub fn profiles_for(audiences: &[Audience]) -> Vec<Profile> {
    if audiences.is_empty() {
        return starter_profiles();
    }
    audiences
        .iter()
        .map(|a| Profile {
            id: a.id.clone(),
            name: a.name.clone(),
            who: a.who.clone().or_else(|| a.short.clone()).unwrap_or_default(),
            part: a.part.clone().unwrap_or_default(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ideas::store::StoreIdea;

    fn row(title: &str, state: IdeaState, by: Option<&str>) -> StoreIdea {
        serde_json::from_value(json!({
            "id": title, "title": title, "state": match state {
                IdeaState::Waiting => "waiting", IdeaState::Accepted => "accepted",
                IdeaState::Later => "later", IdeaState::Never => "never" },
            "at": 1, "addedBy": by,
        }))
        .unwrap()
    }

    fn item(title: &str, status: ItemStatus, pr: Option<&str>) -> Item {
        Item {
            id: title.into(),
            title: title.into(),
            status,
            pr_url: pr.map(String::from),
            ..Item::default()
        }
    }

    #[test]
    fn round_context_sorts_titles_by_state_and_who_added_them() {
        let ideas = StoreIdeas {
            round: None,
            ideas: vec![
                row("a", IdeaState::Accepted, None),
                row("l", IdeaState::Later, None),
                row("n", IdeaState::Never, None),
                row("w", IdeaState::Waiting, None),
                row("p", IdeaState::Waiting, Some("sam")),
            ],
        };
        let live: Vec<String> = (0..15).map(|i| format!("t{i}")).collect();
        let c = round_context(&live, &ideas);
        assert_eq!(c["accepted"], json!(["a"]));
        assert_eq!(c["later"], json!(["l"]));
        assert_eq!(c["never"], json!(["n"]));
        assert_eq!(c["waiting"], json!(["w"]));
        assert_eq!(c["people"], json!(["p"]));
        assert_eq!(c["live"].as_array().unwrap().len(), 12);
        assert_eq!(c["live"][0], "t0");
    }

    #[test]
    fn live_titles_order_by_merge_time_with_unmatched_last_and_skip_unfinished() {
        let items = vec![
            item("old", ItemStatus::OnMain, Some("u1")),
            item("none", ItemStatus::InProduction, Some("missing")),
            item("busy", ItemStatus::Building, Some("u3")),
            item("new", ItemStatus::InProduction, Some("u2")),
        ];
        let merged = HashMap::from([
            ("u1".to_string(), "2026-01-01T00:00:00Z".to_string()),
            ("u2".to_string(), "2026-02-01T00:00:00Z".to_string()),
            ("u3".to_string(), "2026-03-01T00:00:00Z".to_string()),
        ]);
        assert_eq!(live_titles(&items, &merged), ["new", "old", "none"]);
    }

    #[tokio::test]
    async fn gather_live_reads_gh_and_orders_items() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("prs.json");
        std::fs::write(
            &out,
            r#"[{"url":"u1","mergedAt":"2026-01-01T00:00:00Z"},{"url":"u2","mergedAt":"2026-02-01T00:00:00Z"}]"#,
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
            item("first", ItemStatus::OnMain, Some("u1")),
            item("second", ItemStatus::OnMain, Some("u2")),
        ];
        let live = gather_live(&gh, dir.path(), "x/y", "main", &items).await.unwrap();
        assert_eq!(live, ["second", "first"]);
    }

    #[test]
    fn profiles_map_store_audiences_and_fall_back_to_the_starter_set() {
        let a = Audience {
            id: "buyer".into(),
            name: "Buyer".into(),
            short: Some("short".into()),
            who: Some("Buys things.".into()),
            part: Some("shop".into()),
        };
        let p = profiles_for(&[a]);
        assert_eq!(
            (p[0].id.as_str(), p[0].who.as_str(), p[0].part.as_str()),
            ("buyer", "Buys things.", "shop")
        );
        assert_eq!(profiles_for(&[]).len(), 8);
    }
}
