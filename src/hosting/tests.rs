use super::*;
use crate::backlog::deploy::{RESULT_MAX_BYTES, checked_result};
use serde_json::json;

/// A temp repo with each path's content written (parent folders made as needed).
fn repo(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (path, body) in files {
        let full = dir.path().join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, body).unwrap();
    }
    dir
}

fn pkg(deps: &str) -> String {
    format!(r#"{{ "dependencies": {{ {deps} }} }}"#)
}

fn found(files: &[(&str, &str)]) -> Found {
    detect_hosting(repo(files).path())
}

const NONE: Found = Found {
    back_end: false,
    app: None,
};

#[test]
fn a_plain_site_has_no_back_end_and_is_not_an_app() {
    assert_eq!(found(&[("index.html", "<p>hi</p>")]), NONE);
    assert_eq!(found(&[("package.json", &pkg(r#""vite": "5""#))]), NONE);
    // A Pages-only wrangler.toml has no Worker entry.
    assert_eq!(found(&[("wrangler.toml", "pages_build_output_dir = \"dist\"\n")]), NONE);
    // Neither does an unrelated key that merely ends in main.
    assert_eq!(found(&[("wrangler.toml", "domain = \"x\"\n")]), NONE);
    assert_eq!(found(&[]), NONE);
}

#[test]
fn a_server_dependency_a_worker_an_api_folder_or_an_exposed_port_is_a_back_end() {
    let cases: [&[(&str, &str)]; 12] = [
        &[("package.json", r#"{ "dependencies": { "hono": "4" } }"#)],
        &[("package.json", r#"{ "devDependencies": { "express": "4" } }"#)],
        &[("wrangler.toml", "main = \"src/index.ts\"\n")],
        &[("wrangler.jsonc", r#"{ "main": "src/index.ts" }"#)],
        &[("api/hello.js", "")],
        &[("server/index.js", "")],
        &[("netlify/functions/a.js", "")],
        &[("pages/api/a.ts", "")],
        &[("app/api/route.ts", "")],
        &[("src/app/api/route.ts", "")],
        &[("Dockerfile", "FROM node\nEXPOSE 8080\n")],
        &[("requirements.txt", "Flask==3.0\n")],
    ];
    for files in cases {
        assert!(found(files).back_end, "{files:?}");
    }
    assert!(found(&[("requirements.txt", "  fastapi\n")]).back_end);
    assert!(found(&[("requirements.txt", "django>=5\n")]).back_end);
}

#[test]
fn a_dockerfile_without_expose_and_a_python_file_without_a_server_are_not_a_back_end() {
    assert!(!found(&[("Dockerfile", "FROM node\n")]).back_end);
    assert!(!found(&[("requirements.txt", "requests\n")]).back_end);
}

#[test]
fn a_workspace_package_counts() {
    let files = [
        ("package.json", r#"{ "workspaces": ["apps/*", "tools/cli"] }"#),
        ("apps/web/package.json", r#"{ "dependencies": { "react": "19" } }"#),
        ("apps/api/package.json", r#"{ "dependencies": { "fastify": "5" } }"#),
        ("tools/cli/package.json", r#"{ "dependencies": { "electron": "33" } }"#),
    ];
    assert_eq!(
        found(&files),
        Found {
            back_end: true,
            app: Some(App::Desktop)
        }
    );
    let object_form = [
        ("package.json", r#"{ "workspaces": { "packages": ["apps/*"] } }"#),
        ("apps/m/package.json", r#"{ "dependencies": { "expo": "52" } }"#),
    ];
    assert_eq!(found(&object_form).app, Some(App::Mobile));
}

#[test]
fn expo_or_react_native_is_a_phone_app_and_electron_or_tauri_a_desktop_app() {
    assert_eq!(
        found(&[("package.json", &pkg(r#""expo": "52""#))]).app,
        Some(App::Mobile)
    );
    assert_eq!(
        found(&[("package.json", &pkg(r#""react-native": "0.76""#))]).app,
        Some(App::Mobile)
    );
    assert_eq!(
        found(&[("package.json", &pkg(r#""electron": "33""#))]).app,
        Some(App::Desktop)
    );
    assert_eq!(
        found(&[("package.json", &pkg(r#""@tauri-apps/api": "2""#))]).app,
        Some(App::Desktop)
    );
    assert_eq!(found(&[("src-tauri/tauri.conf.json", "{}")]).app, Some(App::Desktop));
    // Desktop wins over mobile, as in the TypeScript.
    assert_eq!(
        found(&[("package.json", &pkg(r#""electron": "33", "expo": "52""#))]).app,
        Some(App::Desktop)
    );
}

/// A checkout under a folder with spaces, named with a trailing separator (as a Windows config often writes one),
/// and a back end folder two levels down: found by components, not a `/` joined string.
#[test]
fn a_path_with_spaces_a_trailing_separator_and_a_nested_folder_reads() {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("My Projects").join("site");
    let nested: std::path::PathBuf = ["netlify", "functions"].iter().collect();
    std::fs::create_dir_all(root.join(nested)).unwrap();
    let mut typed = root.clone().into_os_string();
    typed.push(std::path::MAIN_SEPARATOR_STR);
    assert!(detect_hosting(std::path::Path::new(&typed)).back_end);
}

#[test]
fn a_repo_that_is_not_there_scans_as_nothing() {
    assert_eq!(detect_hosting(std::path::Path::new("no-such-folder-for-hosting")), NONE);
}

fn project(t: ProjectType, public: bool, back_end: bool, app: Option<App>) -> Project {
    Project {
        project_type: t,
        public,
        back_end,
        app,
    }
}

/// The default, then the alternatives, as ids.
fn names(p: &Project) -> Vec<&'static str> {
    let pick = pick_hosting(p);
    std::iter::once(pick.default)
        .chain(pick.alternatives)
        .map(Id::as_str)
        .collect()
}

#[test]
fn a_static_site_in_a_public_repo_goes_on_github_pages_first() {
    for t in [ProjectType::Web, ProjectType::Other] {
        let p = project(t, true, false, None);
        assert_eq!(names(&p), ["github-pages", "cloudflare-pages", "netlify"]);
        assert_eq!(pick_hosting(&p).shape, Shape::Static);
        assert_eq!(pick_hosting(&p).database, None);
    }
}

#[test]
fn the_same_site_in_a_private_repo_goes_on_cloudflare_pages() {
    let p = project(ProjectType::Web, false, false, None);
    assert_eq!(names(&p), ["cloudflare-pages", "netlify"]);
}

#[test]
fn a_web_app_with_a_back_end_goes_on_workers_with_d1_for_its_database() {
    for p in [
        project(ProjectType::Web, true, true, None),
        project(ProjectType::Server, true, false, None),
        project(ProjectType::Other, false, true, None),
    ] {
        let pick = pick_hosting(&p);
        assert_eq!(pick.shape, Shape::BackEnd);
        assert_eq!(names(&p), ["cloudflare-workers", "render", "cloud-run"]);
        let db = pick.database.unwrap();
        assert_eq!(
            [db.default].into_iter().chain(db.alternatives).collect::<Vec<_>>(),
            [Id::CloudflareD1, Id::Supabase, Id::Neon, Id::Turso]
        );
    }
}

#[test]
fn a_game_goes_on_itch_with_github_pages_only_from_a_public_repo() {
    assert_eq!(
        names(&project(ProjectType::Game, true, false, None)),
        ["itch", "github-pages"]
    );
    assert_eq!(names(&project(ProjectType::Game, false, true, None)), ["itch"]);
}

#[test]
fn a_phone_app_goes_on_eas_and_a_desktop_app_or_tool_on_github_releases() {
    assert_eq!(
        names(&project(ProjectType::Web, true, false, Some(App::Mobile))),
        ["eas"]
    );
    assert_eq!(
        names(&project(ProjectType::Other, true, true, Some(App::Desktop))),
        ["github-releases", "itch"]
    );
    assert_eq!(
        names(&project(ProjectType::Cli, false, false, None)),
        ["github-releases", "itch"]
    );
}

#[test]
fn the_provider_ids_are_the_sites_thirteen_and_parse_back() {
    let ids: Vec<_> = Id::ALL.iter().map(|i| i.as_str()).collect();
    assert_eq!(
        ids,
        [
            "github-pages",
            "cloudflare-pages",
            "netlify",
            "cloudflare-workers",
            "render",
            "cloud-run",
            "cloudflare-d1",
            "supabase",
            "neon",
            "turso",
            "itch",
            "eas",
            "github-releases"
        ]
    );
    for i in Id::ALL {
        assert_eq!(Id::parse(i.as_str()), Some(i));
    }
    assert_eq!(Id::parse("vercel"), None);
    assert_eq!(Id::parse("GitHub-Pages"), None);
    assert_eq!(Id::parse(""), None);
}

#[test]
fn the_result_is_the_project_and_the_pick_by_id() {
    let p = project(ProjectType::Web, true, true, Some(App::Mobile));
    assert_eq!(
        serde_json::to_value(result(&project(ProjectType::Server, false, true, None))).unwrap(),
        json!({
            "project": { "type": "server", "public": false, "backEnd": true, "app": null },
            "pick": {
                "shape": "backEnd",
                "default": "cloudflare-workers",
                "alternatives": ["render", "cloud-run"],
                "database": { "default": "cloudflare-d1", "alternatives": ["supabase", "neon", "turso"] },
            },
        })
    );
    assert_eq!(result(&p)["pick"]["shape"], "mobile");
}

/// Every shape the pick can answer, with the worst-case scan, is far under the store's 4 KB and has no key the store
/// refuses as a secret.
#[test]
fn every_result_shape_is_well_under_the_stores_cap() {
    let types = [
        ProjectType::Game,
        ProjectType::Web,
        ProjectType::Server,
        ProjectType::Cli,
        ProjectType::Other,
    ];
    let mut biggest = 0;
    for t in types {
        for public in [true, false] {
            for back_end in [true, false] {
                for app in [None, Some(App::Mobile), Some(App::Desktop)] {
                    let r = result(&project(t, public, back_end, app));
                    let size = serde_json::to_vec(&r).unwrap().len();
                    biggest = biggest.max(size);
                    assert!(
                        size < RESULT_MAX_BYTES / 4,
                        "{t:?} {public} {back_end} {app:?}: {size} bytes"
                    );
                    assert!(checked_result(r).is_ok());
                }
            }
        }
    }
    assert!(biggest > 0);
}

#[test]
fn the_project_is_public_only_when_github_says_so() {
    let dir = repo(&[]);
    for (private, public) in [(Some(false), true), (Some(true), false), (None, false)] {
        assert_eq!(project_of(ProjectType::Web, dir.path(), private).public, public);
    }
}

#[test]
fn private_is_anything_but_public_and_unknown_when_gh_cannot_say() {
    use private::private_from;
    assert_eq!(private_from(br#"{"visibility":"PUBLIC"}"#), Some(false));
    assert_eq!(private_from(br#"{"visibility":"PRIVATE"}"#), Some(true));
    assert_eq!(private_from(br#"{"visibility":"INTERNAL"}"#), Some(true));
    assert_eq!(private_from(br#"{"visibility":7}"#), None);
    assert_eq!(private_from(br#"{}"#), None);
    assert_eq!(private_from(b"not json"), None);
}

/// Every provider's item, from the TypeScript's own `prepareItem` for the project "Trailhead", byte for byte.
#[test]
fn the_preparing_item_is_the_typescripts_word_for_word() {
    let golden: Vec<serde_json::Value> = serde_json::from_str(include_str!("prepare_golden.json")).unwrap();
    assert_eq!(golden.len(), Id::ALL.len());
    for (id, want) in Id::ALL.into_iter().zip(golden) {
        let item = prepare_item(provider(id), "Trailhead");
        assert_eq!(serde_json::to_value(&item).unwrap(), want, "{}", id.as_str());
        assert_eq!(item.id, format!("go-live-{}", id.as_str()));
        assert!(item.remaining.len() <= 4000);
    }
}

#[test]
fn a_website_item_adds_version_json_and_the_others_do_not() {
    let with: Vec<_> = Id::ALL
        .into_iter()
        .filter(|i| provider(*i).commit.is_some())
        .map(Id::as_str)
        .collect();
    assert_eq!(
        with,
        [
            "github-pages",
            "cloudflare-pages",
            "netlify",
            "cloudflare-workers",
            "render"
        ]
    );
    let pages = prepare_item(provider(Id::CloudflarePages), "Trailhead").remaining;
    assert!(
        pages.contains("/version.json")
            && pages.contains("CF_PAGES_COMMIT_SHA")
            && pages.ends_with("Never deploy, and never add a token, password or secret to the repo.")
    );
    for id in [Id::CloudflareD1, Id::Itch, Id::Eas, Id::GithubReleases, Id::CloudRun] {
        assert!(
            !prepare_item(provider(id), "Trailhead")
                .remaining
                .contains("version.json")
        );
    }
}

#[test]
fn an_unknown_provider_is_refused_in_the_typescripts_words() {
    assert_eq!(provider_of(Some("itch")).unwrap().name, "itch.io");
    for (given, said) in [
        (Some("vercel"), "vercel"),
        (Some("GitHub-Pages"), "GitHub-Pages"),
        (Some(""), ""),
        (None, "undefined"),
    ] {
        assert_eq!(
            provider_of(given).err().unwrap(),
            format!("There's no hosting provider \"{said}\".")
        );
    }
}

#[test]
fn the_project_is_the_repos_own_name() {
    assert_eq!(project_name("ann/trailhead"), "trailhead");
    assert_eq!(project_name("trailhead"), "trailhead");
    assert_eq!(project_name("ann/"), "ann/");
    assert_eq!(project_name(""), "");
}
