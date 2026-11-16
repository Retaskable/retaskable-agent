//! Which provider to offer a project (`pickHosting` in `shared/hosting.ts`, go-live.md section 2): pure, from what the
//! project is and whether its repo is public. The answer is ids only; the site looks the providers up.
use serde::Serialize;

use super::Id::{self, *};
use super::{App, Project};
use crate::config::ProjectType;

/// Which row of go-live.md section 2's table the project is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Shape {
    Static,
    BackEnd,
    Game,
    Mobile,
    Desktop,
}

/// A database offered beside a back end.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Database {
    pub default: Id,
    pub alternatives: Vec<Id>,
}

/// The chosen providers, by id: `HostingPick` with each provider object cut to its id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Pick {
    pub shape: Shape,
    pub default: Id,
    pub alternatives: Vec<Id>,
    pub database: Option<Database>,
}

/// The project's row: its type first, then what the repo scan found.
fn shape_of(p: &Project) -> Shape {
    match p.project_type {
        ProjectType::Game => Shape::Game,
        ProjectType::Server => Shape::BackEnd,
        ProjectType::Cli => Shape::Desktop,
        ProjectType::Web | ProjectType::Other => match p.app {
            Some(App::Mobile) => Shape::Mobile,
            Some(App::Desktop) => Shape::Desktop,
            None if p.back_end => Shape::BackEnd,
            None => Shape::Static,
        },
    }
}

/// The default and the alternatives for a project.
pub fn pick_hosting(p: &Project) -> Pick {
    let shape = shape_of(p);
    let pick = |default: Id, alternatives: &[Id]| Pick {
        shape,
        default,
        alternatives: alternatives.to_vec(),
        database: None,
    };
    match shape {
        // GitHub Pages on a private repo needs a paid GitHub plan.
        Shape::Static if p.public => pick(GithubPages, &[CloudflarePages, Netlify]),
        Shape::Static => pick(CloudflarePages, &[Netlify]),
        Shape::BackEnd => Pick {
            database: Some(Database {
                default: CloudflareD1,
                alternatives: vec![Supabase, Neon, Turso],
            }),
            ..pick(CloudflareWorkers, &[Render, CloudRun])
        },
        Shape::Game if p.public => pick(Itch, &[GithubPages]),
        Shape::Game => pick(Itch, &[]),
        Shape::Mobile => pick(Eas, &[]),
        Shape::Desktop => pick(GithubReleases, &[Itch]),
    }
}
