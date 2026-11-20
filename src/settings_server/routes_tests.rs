//! The HTTP port serves only `GET /api/health` and the settings page (docs/specs/programs.md): reads, and saves of a
//! settings file. It never carries a command to a program. This fails when a route is added, so adding one is a
//! decision somebody makes here, in review, rather than a side effect. It reads the routes out of the source (axum
//! can't list them), for everything that serves HTTP in this crate.
use std::path::{Path, PathBuf};

/// Every route, as `METHOD path`. A saved setting is a POST to `/api/settings...`; nothing here starts, stops,
/// retries or runs a program's work. The two `.../link` POSTs (agent-settings.md section 4.6, **[Decided 2026-11-16]**)
/// are the **Link** button: they run a device flow in this process that, once a person approves it on the store's own
/// page, saves a token, as a saved setting does. They run no program and send it nothing. The `/api/projects`
/// routes (agent-settings.md section 4.7, **[Decided 2026-11-16]**) are **Add project**: three reads (the projects
/// added and where an add is, the person's GitHub owners, an owner's repos) and one POST that is a settings change, not
/// a command: it clones a repo this person picked, writes its entry into `config.json` and starts it paused. Behind the
/// session, CSRF, Origin, Host and this-computer checks; it runs no program and sends none anything. A fifth,
/// `POST /api/projects/{id}/paused` `{ paused }`, is **Pause / Resume** of this computer's work on a project: a setting
/// (this computer's pause, as the site's `pause`/`resume` with `computer: true` set it), named as one, with the same
/// checks. It starts no work and stops none directly: the next pass reads the pause.
///
/// The setup wizard's routes (docs/specs/setup-wizard.md section 3): `GET /api/wizard` reads, `POST /api/wizard/github` and
/// `/claude` save a token (a secret, as `POST /api/settings` does, then check it), `POST /api/wizard/ci` is the CI
/// runner's slots (a setting, as the programs panel's save is), and `POST /api/wizard/claude/test` runs the one fixed,
/// argument-less probe (a tiny `claude -p` call, like the setup checks' own probes): nothing in a request says what runs.
const ALLOWED: [&str; 25] = [
    "GET /api/health",
    "GET /settings",
    "POST /api/settings/session",
    "GET /api/settings",
    "POST /api/settings",
    "GET /api/settings/status",
    "GET /api/settings/programs",
    "POST /api/settings/programs/{program}",
    "POST /api/settings/programs/{program}/link",
    "POST /api/settings/restart",
    "GET /api/link",
    "POST /api/link/{target}",
    "GET /api/backlog/link",
    // A redirect to the page itself: people type the bare address (setup-wizard.md).
    "GET /",
    "GET /api/profile",
    "GET /api/projects",
    "POST /api/projects",
    "GET /api/projects/owners",
    "GET /api/projects/repos",
    "POST /api/projects/{id}/paused",
    "GET /api/wizard",
    "POST /api/wizard/github",
    "POST /api/wizard/claude",
    "POST /api/wizard/claude/test",
    "POST /api/wizard/ci",
];

/// What the CI runner's own port (`retaskable-ci`, `agent_ci/serve.rs`) serves, on a computer with no code agent: the
/// health check, its trimmed page, signing in and the programs panel. None of the code agent's own settings or its
/// restart.
const CI_ALLOWED: [&str; 6] = [
    "GET /api/health",
    "GET /settings",
    "POST /api/settings/session",
    "GET /api/settings/programs",
    "POST /api/settings/programs/{program}",
    "POST /api/settings/programs/{program}/link",
];

/// The one route that makes the code agent itself do something: it exits so its supervisor starts it again, to apply a
/// saved port. It acts on the process serving the page and on nothing else, and it is not new. No route may do more.
const SELF_RESTART: &str = "POST /api/settings/restart";

/// Words in a path that mean "do this", not "this setting".
const COMMAND_WORDS: [&str; 12] = [
    "start", "stop", "run", "exec", "kill", "spawn", "command", "cancel", "retry", "trigger", "dispatch", "rpc",
];

fn sources(dir: &Path, into: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if path.is_dir() {
            if name != "tests" {
                sources(&path, into);
            }
        } else if name.ends_with(".rs") && !name.ends_with("tests.rs") {
            into.push(path);
        }
    }
}

/// `METHOD path` for every `.route("path", method(..).method(..))` in `text`.
fn routes_in(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for (at, _) in text.match_indices(".route(") {
        let rest = &text[at + ".route(".len()..];
        // The path is the first string literal; its handlers follow up to the call's closing parenthesis.
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix('"') else { continue };
        let Some(end) = rest.find('"') else { continue };
        let path = &rest[..end];
        let mut depth = 1;
        let mut args = String::new();
        for c in rest[end + 1..].chars() {
            match c {
                '(' => depth += 1,
                ')' => depth -= 1,
                _ => {}
            }
            if depth == 0 {
                break;
            }
            args.push(c);
        }
        for method in ["get", "post", "put", "delete", "patch"] {
            let mut from = 0;
            while let Some(i) = args[from..].find(&format!("{method}(")) {
                let before = args[..from + i].chars().next_back();
                if !before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                    found.push(format!("{} {path}", method.to_uppercase()));
                }
                from += i + method.len();
            }
        }
    }
    found
}

fn routes_of(files: &[PathBuf]) -> Vec<String> {
    let mut routes: Vec<String> = files
        .iter()
        .flat_map(|f| routes_in(&std::fs::read_to_string(f).unwrap()))
        .collect();
    routes.sort();
    routes.dedup();
    routes
}

fn all_routes() -> Vec<String> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = vec![src.join("api.rs"), src.join("bin").join("ci.rs")];
    sources(&src.join("settings_server"), &mut files);
    sources(&src.join("page_host"), &mut files);
    sources(&src.join("agent_ci"), &mut files);
    routes_of(&files)
}

/// The CI runner's routes: its own files and the shared page host's, and nothing of the code agent's.
fn ci_routes() -> Vec<String> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = vec![src.join("bin").join("ci.rs")];
    sources(&src.join("page_host"), &mut files);
    sources(&src.join("agent_ci"), &mut files);
    routes_of(&files)
}

#[test]
fn the_port_serves_only_the_health_check_and_the_settings_page() {
    let mut want: Vec<String> = ALLOWED.iter().map(|s| s.to_string()).collect();
    want.sort();
    assert_eq!(
        all_routes(),
        want,
        "a route was added or removed: the HTTP port serves only GET /api/health and the settings page, never a \
         command (docs/specs/programs.md). If this is a read or a settings save, add it to ALLOWED."
    );
}

#[test]
fn the_ci_runners_port_serves_only_its_page_sign_in_the_programs_panel_and_health() {
    let mut want: Vec<String> = CI_ALLOWED.iter().map(|s| s.to_string()).collect();
    want.sort();
    assert_eq!(
        ci_routes(),
        want,
        "a route was added to the CI runner's port: it serves only its settings page and GET /api/health, never a \
         command (docs/specs/programs.md, agent-ci.md section 3.9)."
    );
    assert!(
        CI_ALLOWED.iter().all(|r| ALLOWED.contains(r)),
        "the CI runner's routes are a subset of the allowlist"
    );
}

#[test]
fn no_route_is_named_like_a_command() {
    for route in ALLOWED.iter().chain(&CI_ALLOWED).filter(|r| **r != SELF_RESTART) {
        let path = route.split_once(' ').unwrap().1;
        for word in COMMAND_WORDS {
            assert!(
                !path.split(['/', '-', '_']).any(|part| part == word),
                "{route} reads as a command ({word}): the settings page edits settings and shows status, nothing more"
            );
        }
    }
}

#[test]
fn the_scan_sees_a_route_and_each_of_its_methods() {
    let text = r#"Router::new().route("/a", get(x).post(y)).route("/b/{c}", post(z)).route("/d", get(forget(1)))"#;
    assert_eq!(routes_in(text), ["GET /a", "POST /a", "POST /b/{c}", "GET /d"]);
}
