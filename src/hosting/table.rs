//! What the preparing item says about each provider (`HOSTING` in `shared/hosting.ts`): the provider's name, what its
//! implementer adds to the repo, and the variable its build sets to the commit it builds. Only what the item's text
//! needs; the limits, steps and pictures the site shows are the site's (Retaskable.com#3858). Word for word the
//! TypeScript's, so the item a person gets is the same from either agent (`prepare_golden.json` is its output).
use super::Id;

/// One provider's part of the preparing item.
pub struct Provider {
    pub id: Id,
    pub name: &'static str,
    /// What the item's implementer adds to the repo for it (empty: nothing to add).
    pub prepare: &'static str,
    /// The variable the provider's build sets to the commit it builds; none when its address can't serve a
    /// `/version.json` (a database, a download, a store build, an itch.io page, a Cloud Run build not told its commit).
    pub commit: Option<&'static str>,
}

pub fn provider(id: Id) -> &'static Provider {
    PROVIDERS.iter().find(|p| p.id == id).expect("every id has a provider")
}

static PROVIDERS: [Provider; 13] = [
    Provider {
        id: Id::GithubPages,
        name: "GitHub Pages",
        prepare: "Add a GitHub Actions workflow that, on each push to the default branch, builds the site (plain HTML needs no build: publish its folder as it is) and publishes it with actions/upload-pages-artifact and actions/deploy-pages (free on public repos). Make links and asset paths work under the repo's own path (https://<owner>.github.io/<repo>/).",
        commit: Some("GITHUB_SHA"),
    },
    Provider {
        id: Id::CloudflarePages,
        name: "Cloudflare Pages",
        prepare: "Add a wrangler.toml with pages_build_output_dir set to the folder the build writes, and make sure package.json has the build command Cloudflare should run. Write both in the README's deploy section.",
        commit: Some("CF_PAGES_COMMIT_SHA"),
    },
    Provider {
        id: Id::Netlify,
        name: "Netlify",
        prepare: "Add a netlify.toml with the build command and the publish folder the build writes.",
        commit: Some("COMMIT_REF"),
    },
    Provider {
        id: Id::CloudflareWorkers,
        name: "Cloudflare Workers",
        prepare: "Add a wrangler.toml (or wrangler.jsonc) naming the Worker's entry and, for a front end, its static assets folder, and a build command in package.json. Bindings (a D1 database, secrets) are listed in it but their values are set on Cloudflare by the person, never committed.",
        commit: Some("WORKERS_CI_COMMIT_SHA"),
    },
    Provider {
        id: Id::Render,
        name: "Render",
        prepare: "Add a render.yaml with the service's build and start commands and the free plan, and make the server listen on the PORT environment variable.",
        commit: Some("RENDER_GIT_COMMIT"),
    },
    Provider {
        id: Id::CloudRun,
        name: "Google Cloud Run",
        prepare: "Add a Dockerfile that builds the app and starts the server listening on the PORT environment variable.",
        commit: None,
    },
    Provider {
        id: Id::CloudflareD1,
        name: "Cloudflare D1",
        prepare: "Bind a D1 database in wrangler.toml and add its schema as migrations in the repo.",
        commit: None,
    },
    Provider {
        id: Id::Supabase,
        name: "Supabase",
        prepare: "Read the database's address from an environment variable and add the schema as migrations in the repo.",
        commit: None,
    },
    Provider {
        id: Id::Neon,
        name: "Neon",
        prepare: "Read the database's address from an environment variable and add the schema as migrations in the repo.",
        commit: None,
    },
    Provider {
        id: Id::Turso,
        name: "Turso",
        prepare: "Read the database's address and token from environment variables and add the schema as migrations in the repo.",
        commit: None,
    },
    Provider {
        id: Id::Itch,
        name: "itch.io",
        prepare: "Add a script that makes an itch.io-ready web build: a zip with index.html at its top level, using relative paths only.",
        commit: None,
    },
    Provider {
        id: Id::Eas,
        name: "Expo (EAS) free tier",
        prepare: "Add an eas.json with a preview build profile for sharing test builds (internal distribution).",
        commit: None,
    },
    Provider {
        id: Id::GithubReleases,
        name: "GitHub Releases",
        prepare: "Add a GitHub Actions workflow that, when a release is published, builds the app for Windows, macOS and Linux and attaches the files to it.",
        commit: None,
    },
];
