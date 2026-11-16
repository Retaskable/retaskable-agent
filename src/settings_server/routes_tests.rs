//! The HTTP port serves only `GET /api/health` and the settings page (docs/specs/programs.md): reads, and saves of a
//! settings file. It never carries a command to a program. This fails when a route is added, so adding one is a
//! decision somebody makes here, in review, rather than a side effect. It reads the routes out of the source (axum
//! can't list them), for everything that serves HTTP in this crate.
use std::path::{Path, PathBuf};

/// Every route, as `METHOD path`. A saved setting is a POST to `/api/settings...`; nothing here starts, stops,
/// retries or runs a program's work.
const ALLOWED: [&str; 12] = [
    "GET /api/health",
    "GET /settings",
    "POST /api/settings/session",
    "GET /api/settings",
    "POST /api/settings",
    "GET /api/settings/status",
    "GET /api/settings/programs",
    "POST /api/settings/programs/{program}",
    "POST /api/settings/restart",
    "GET /api/link",
    "GET /api/backlog/link",
    "GET /api/profile",
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

fn all_routes() -> Vec<String> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = vec![src.join("api.rs")];
    sources(&src.join("settings_server"), &mut files);
    sources(&src.join("page_host"), &mut files);
    let mut routes: Vec<String> = files
        .iter()
        .flat_map(|f| routes_in(&std::fs::read_to_string(f).unwrap()))
        .collect();
    routes.sort();
    routes
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
fn no_route_is_named_like_a_command() {
    for route in ALLOWED.iter().filter(|r| **r != SELF_RESTART) {
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
