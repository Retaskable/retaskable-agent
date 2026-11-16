//! The three programs (docs/specs/programs.md) stay separable: the code agent (`retaskable`), the Chat Agent
//! (`src/chat`, `src/bin/chat_agent.rs`) and the CI runner (`src/agent_ci`, `src/bin/ci.rs`) coordinate only
//! through the shared store, so none of them names another's module. What they share lives in neutral modules.
//! This scans the source for `crate::<module>` and `retaskable_agent::<module>` paths, comments skipped.
use std::path::{Path, PathBuf};

const CHAT: &[&str] = &["orchestrator", "agent_ci"];
const CI: &[&str] = &["orchestrator", "chat"];
const CODE: &[&str] = &["chat", "agent_ci"];

/// Which program a source file (relative to `src`, as path components) belongs to, with the modules it may not name.
/// `None` for the files that name every module on purpose: the module list, and tests that cross programs.
fn forbidden(rel: &Path) -> Option<&'static [&'static str]> {
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let first = parts.first().map(String::as_str).unwrap_or_default();
    let file = parts.last().map(String::as_str).unwrap_or_default();
    match (first, parts.len()) {
        ("chat", _) => Some(CHAT),
        ("agent_ci", _) => Some(CI),
        ("bin", 2) if file == "chat_agent.rs" => Some(CHAT),
        ("bin", 2) if file == "ci.rs" => Some(CI),
        ("lib.rs" | "host_call_sites_tests.rs" | "program_boundaries_tests.rs", 1) => None,
        _ => Some(CODE),
    }
}

/// The first segment of every `crate::`/`retaskable_agent::` path on the line, including each top-level
/// item of a `crate::{a, b::c}` group.
fn first_segments(line: &str) -> Vec<String> {
    let mut found = Vec::new();
    for root in ["crate::", "retaskable_agent::"] {
        for (at, _) in line.match_indices(root) {
            if line[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_')
            {
                continue;
            }
            let rest = &line[at + root.len()..];
            if let Some(group) = rest.strip_prefix('{') {
                let mut depth = 0;
                let mut item = String::new();
                for c in group.chars() {
                    match c {
                        '{' => depth += 1,
                        '}' if depth == 0 => break,
                        '}' => depth -= 1,
                        _ => {}
                    }
                    if depth == 0 && c == ',' {
                        found.push(item.trim().to_string());
                        item.clear();
                    } else if depth == 0 || c == '{' {
                        item.push(c);
                    }
                }
                found.push(item.trim().to_string());
            } else {
                found.push(rest.to_string());
            }
        }
    }
    found
        .into_iter()
        .map(|s| s.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect())
        .collect()
}

/// The `(file, line number, module)` of every forbidden path in `text`, for a file that may not name `banned`.
fn violations(file: &Path, text: &str, banned: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        if line.trim_start().starts_with("//") {
            continue;
        }
        for seg in first_segments(line) {
            if banned.contains(&seg.as_str()) {
                out.push(format!("{}:{}: names `{seg}`", file.display(), n + 1));
            }
        }
    }
    out
}

fn rust_files(dir: &Path, into: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            rust_files(&p, into);
        } else if p.extension().is_some_and(|e| e == "rs") {
            into.push(p);
        }
    }
}

fn scan(src: &Path) -> Vec<String> {
    let mut files = Vec::new();
    rust_files(src, &mut files);
    let mut bad = Vec::new();
    for f in files {
        let rel = f.strip_prefix(src).unwrap();
        if let Some(banned) = forbidden(rel) {
            bad.extend(violations(rel, &std::fs::read_to_string(&f).unwrap(), banned));
        }
    }
    bad
}

#[test]
fn no_program_names_another_programs_module() {
    let bad = scan(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"));
    assert!(
        bad.is_empty(),
        "move what is shared into a neutral module (docs/specs/programs.md):\n{}",
        bad.join("\n")
    );
}

#[test]
fn the_scan_sees_each_way_of_naming_a_module() {
    let text = "use crate::orchestrator::live;\nuse crate::{text, orchestrator::x, a::{b, c}};\n\
                // use crate::orchestrator::y;\nlet z = retaskable_agent::orchestrator::z();\nuse xcrate::orchestrator;\n";
    let got = violations(Path::new("f.rs"), text, CHAT);
    assert_eq!(got.len(), 3, "{got:?}");
    assert!(got.iter().any(|g| g.starts_with("f.rs:1:")) && got.iter().any(|g| g.starts_with("f.rs:2:")));
    assert!(got.iter().any(|g| g.starts_with("f.rs:4:")));
}

#[test]
fn each_file_is_held_to_its_own_program_rules() {
    let rule = |p: &str| forbidden(&PathBuf::from_iter(p.split('/')));
    assert_eq!(rule("chat/hub.rs"), Some(CHAT));
    assert_eq!(rule("bin/chat_agent.rs"), Some(CHAT));
    assert_eq!(rule("agent_ci/store/tests.rs"), Some(CI));
    assert_eq!(rule("bin/ci.rs"), Some(CI));
    assert_eq!(rule("main.rs"), Some(CODE));
    assert_eq!(rule("orchestrator/driver.rs"), Some(CODE));
    assert_eq!(rule("lib.rs"), None);
}
