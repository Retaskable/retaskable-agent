//! The user types an ideas prompt names, and filling `prompts/ideas.md` with them
//! (Retaskable.com's `agent/src/ideas.ts` and `shared/audiences.ts`).
use anyhow::{Context as _, Result};
use serde_json::{Value, json};

use crate::config::ProjectType;
use crate::release_notes::{project_words, render_prompt};

/// A user type the prompt names (`Audience`, shared/audiences.ts, trimmed to what the prompt shows):
/// the full audiences book isn't ported yet.
#[derive(Debug, Clone)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub who: String,
    pub part: String,
}

/// Fills `prompts/ideas.md`'s template with this project's words, what prompted the round, how much
/// room is left in the queue, the user types, and the already-gathered `context` (what went live,
/// what was asked, past answers — shared/moreTypes.ts's shape, built by the caller: see the module
/// doc comment).
pub fn build_prompt(
    template: &str,
    project_type: ProjectType,
    github_repo: &str,
    after: &str,
    room: usize,
    profiles: &[Profile],
    context: &Value,
) -> Result<String> {
    let words = project_words(project_type);
    let profiles_json = serde_json::to_string_pretty(
        &profiles
            .iter()
            .map(|p| json!({ "id": p.id, "name": p.name, "who": p.who, "part": p.part }))
            .collect::<Vec<_>>(),
    )
    .context("rendering ideas profiles as json")?;
    let context_json = serde_json::to_string_pretty(context).context("rendering ideas context as json")?;
    render_prompt(
        template,
        &[
            ("projectKind", words.kind.to_string()),
            ("githubRepo", github_repo.to_string()),
            ("after", after.to_string()),
            ("room", room.to_string()),
            ("profiles", profiles_json),
            ("context", context_json),
        ],
    )
}

/// A fixed, starter set of user types (`starterAudiences`, shared/audiences.ts, trimmed to what
/// [`Profile`] needs): a part of the product, and a name and one line for each. Stands in until the
/// real audiences book — one a person can add to and edit — reaches this agent.
pub fn starter_profiles() -> Vec<Profile> {
    [
        ("admin", "Admin", "Runs the admin panel.", "admin"),
        (
            "support",
            "Support",
            "Looks up a customer and fixes an account.",
            "admin",
        ),
        ("shopper", "Shopper", "Buys from the shop.", "shop"),
        ("guest", "Guest", "Looks around the shop before signing in.", "shop"),
        (
            "analyst",
            "Analyst",
            "Reads the reports and checks the numbers.",
            "reports",
        ),
        ("buyer", "Buyer", "Chooses the product for a company.", "enterprise"),
        (
            "account-admin",
            "Account admin",
            "Manages the people on an enterprise account.",
            "enterprise",
        ),
        (
            "infrastructure",
            "Infrastructure",
            "Keeps the servers, builds and deploys running for everyone else.",
            "platform",
        ),
    ]
    .into_iter()
    .map(|(id, name, who, part)| Profile {
        id: id.to_string(),
        name: name.to_string(),
        who: who.to_string(),
        part: part.to_string(),
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;

    #[test]
    fn build_prompt_fills_the_ideas_template() {
        let template = std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("prompts")
                .join("ideas.md"),
        )
        .unwrap();
        let prompt = build_prompt(
            &template,
            ProjectType::Web,
            "x/y",
            "Chores",
            3,
            &[Profile {
                id: "shopper".to_string(),
                name: "Shopper".to_string(),
                who: "Buys things".to_string(),
                part: "customer".to_string(),
            }],
            &json!({ "live": [], "asked": [] }),
        )
        .unwrap();
        assert!(!prompt.contains("{{"));
        assert!(prompt.contains("a web app"));
        assert!(prompt.contains("x/y"));
        assert!(prompt.contains("Chores"));
        assert!(prompt.contains("Shopper"));
    }

    #[test]
    fn build_prompt_fails_on_a_template_with_a_placeholder_it_cant_fill() {
        let err = build_prompt("{{unknown}}", ProjectType::Web, "x/y", "", 1, &[], &json!({})).unwrap_err();
        assert!(err.to_string().contains("unknown"));
    }

    #[test]
    fn starter_profiles_are_each_a_unique_known_part_of_the_product() {
        let parts = ["admin", "shop", "reports", "enterprise", "platform"];
        let profiles = starter_profiles();
        assert!(!profiles.is_empty());
        let mut ids: Vec<&str> = profiles.iter().map(|p| p.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), profiles.len(), "every id is unique");
        for p in &profiles {
            assert!(
                parts.contains(&p.part.as_str()),
                "{} has an unknown part {}",
                p.id,
                p.part
            );
            assert!(!p.name.is_empty());
            assert!(!p.who.is_empty());
        }
    }
}
