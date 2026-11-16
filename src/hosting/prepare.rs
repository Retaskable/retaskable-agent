//! The backlog item choosing a provider adds (`prepareItem` in `shared/hosting.ts`): "Get <project> ready for
//! <provider>". Word for word the TypeScript's (`prepare_golden.json`), so the item is the same from either agent.
use serde::Serialize;

use super::Id;
use super::table::{Provider, provider};

/// The item, as the store's `add` takes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Item {
    pub id: String,
    pub title: String,
    pub remaining: String,
}

/// The provider asked for by id, or the words that refuse it (`providerOf` in `goLive.ts`). A missing id reads as
/// `undefined`, as the TypeScript's `String(id)` does.
pub fn provider_of(id: Option<&str>) -> Result<&'static Provider, String> {
    Id::parse(id.unwrap_or("undefined"))
        .map(provider)
        .ok_or_else(|| format!("There's no hosting provider \"{}\".", id.unwrap_or("undefined")))
}

/// The repo's own name: what follows the last `/` of `owner/name` (the whole of it when there is no name there).
pub fn project_name(github_repo: &str) -> &str {
    match github_repo.rsplit('/').next() {
        Some(name) if !name.is_empty() => name,
        _ => github_repo,
    }
}

pub fn prepare_item(p: &Provider, project: &str) -> Item {
    let version = p.commit.map_or_else(String::new, |commit| {
        format!(
            " So Retaskable can tell when a change is live, make the live site answer /version.json, at the root it is served from, with {{\"sha\":\"<commit>\"}}: the commit it was built from, written during the build from the {commit} variable {} sets (else git rev-parse HEAD), and never cached (Cache-Control: no-store where the host allows it).",
            p.name
        )
    });
    let remaining = if p.prepare.is_empty() {
        format!(
            "Prepare this project to go live on {}, which builds it from this GitHub repo itself.{version}",
            p.name
        )
    } else {
        format!(
            "Prepare this project to go live on {}, which builds it from this GitHub repo itself. {}{version} Never deploy, and never add a token, password or secret to the repo.",
            p.name, p.prepare
        )
    };
    Item {
        id: format!("go-live-{}", p.id.as_str()),
        title: format!("Get {project} ready for {}", p.name),
        remaining,
    }
}
