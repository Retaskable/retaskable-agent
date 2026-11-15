//! Go live's picker read (go-live.md section 3, `hosting` in `agent/src/goLive.ts`): the project's type, whether its
//! repo is public, what its files say (a back end, a phone or desktop app) and the providers chosen for it. A read:
//! nothing is written anywhere, and Retaskable never deploys. The result names the chosen providers by id only; the
//! site looks up their limits and steps from its own table (Retaskable.com#3858), so this computer keeps the ids and
//! the choosing here and none of the display text. The one text it does keep is the item that prepares a repo for a
//! provider ([`prepare_item`]), which needs the provider's name, what to add and its commit variable.
mod ids;
mod pick;
mod prepare;
mod private;
mod scan;
mod table;
#[cfg(test)]
mod tests;

use std::path::Path;

use serde::Serialize;
use serde_json::{Map, Value};

use crate::config::{ProjectConfig, ProjectType};
pub use ids::Id;
pub use pick::{Database, Pick, Shape, pick_hosting};
pub use prepare::{Item, prepare_item, project_name, provider_of};
pub use private::repo_private;
pub use scan::{Found, detect_hosting};
pub use table::{Provider, provider};

/// A phone app (Expo, React Native) or a desktop app (Electron, Tauri).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum App {
    Mobile,
    Desktop,
}

fn type_name<S: serde::Serializer>(t: &ProjectType, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(match t {
        ProjectType::Game => "game",
        ProjectType::Web => "web",
        ProjectType::Server => "server",
        ProjectType::Cli => "cli",
        ProjectType::Other => "other",
    })
}

/// What the pick reads (`HostingProject`): the type, the repo's visibility and the scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    #[serde(rename = "type", serialize_with = "type_name")]
    pub project_type: ProjectType,
    /// GitHub says the repo is public (not when it can't say).
    pub public: bool,
    pub back_end: bool,
    pub app: Option<App>,
}

/// The project as the pick reads it, from its config, its checkout and what GitHub said about its visibility.
pub fn project_of(project_type: ProjectType, repo: &Path, private: Option<bool>) -> Project {
    let Found { back_end, app } = detect_hosting(repo);
    Project {
        project_type,
        public: private == Some(false),
        back_end,
        app,
    }
}

/// The `hosting` request's result: `{ project, pick }`.
pub fn result(project: &Project) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("project".into(), serde_json::to_value(project).unwrap_or_default());
    m.insert(
        "pick".into(),
        serde_json::to_value(pick_hosting(project)).unwrap_or_default(),
    );
    m
}

/// Reads the project: its repo's visibility from `gh`, its files from the checkout.
pub async fn read(cfg: &ProjectConfig) -> Map<String, Value> {
    let private = repo_private(&cfg.github_repo).await;
    result(&project_of(cfg.project_type, Path::new(&cfg.repo_path), private))
}
