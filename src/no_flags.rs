//! The lint that keeps release flags out of agent code (docs/specs/no-agent-flags.md section 3): a test in
//! `cargo test`, so `cargo test` is the check. Flags belong to the app and the Worker; a new agent feature ships On.

/// What a line must not contain, each with the read it stands for.
const BANNED: [&str; 6] = [
    "Flags::",
    "crate::flags",
    "retaskable_agent::flags",
    ".on(\"",
    ".last(\"",
    "RETASKABLE_FLAG_KEY",
];

/// Files allowed to name a banned pattern, each with the reason. A file added needs a reason.
const ALLOWED: [(&str, &str); 1] = [("src/no_flags.rs", "this lint names the patterns it bans")];

const MESSAGE: &str = "Agent code has no release flags: ship the behaviour On. See CLAUDE.md 'No release flags in agent code' and docs/specs/no-agent-flags.md.";

/// One `path:line` per hit over (path, text) pairs, then the message; empty when the files are clean.
fn hits(files: &[(String, String)]) -> Vec<String> {
    let mut out = Vec::new();
    for (path, text) in files {
        if ALLOWED.iter().any(|(allowed, _)| path == allowed) {
            continue;
        }
        for (i, line) in text.lines().enumerate() {
            if BANNED.iter().any(|b| line.contains(b)) {
                out.push(format!("{path}:{}", i + 1));
            }
        }
    }
    if !out.is_empty() {
        out.push(MESSAGE.to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn pair(path: &str, text: &str) -> (String, String) {
        (path.to_string(), text.to_string())
    }

    fn rs_files(dir: &Path, out: &mut Vec<(String, String)>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                rs_files(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let shown = path
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/");
                out.push((shown, std::fs::read_to_string(&path).unwrap()));
            }
        }
    }

    #[test]
    fn every_banned_read_is_a_hit_with_its_line_and_the_message() {
        for bad in [
            "let f = Flags::from_env();",
            "use crate::flags::Flags;",
            "retaskable_agent::flags::x();",
            "if f.on(\"x\").await {}",
            "f.last(\"x\")",
            "std::env::var(\"RETASKABLE_FLAG_KEY\")",
        ] {
            let got = hits(&[pair("src/a.rs", &format!("fn a() {{}}\n{bad}\n"))]);
            assert_eq!(got, ["src/a.rs:2", MESSAGE], "{bad}");
        }
    }

    #[test]
    fn a_clean_file_and_an_allowed_one_are_not() {
        assert!(hits(&[pair("src/a.rs", "fn a() { run(\"--flags\"); }\n")]).is_empty());
        assert!(hits(&[pair("src/no_flags.rs", "Flags::new()")]).is_empty());
    }

    #[test]
    fn agent_code_has_no_release_flags() {
        let mut files = Vec::new();
        rs_files(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
        assert!(!files.is_empty());
        let files: Vec<_> = files
            .into_iter()
            .map(|(p, t)| {
                (
                    p.split_once("/src/")
                        .map_or(p.clone(), |(_, rest)| format!("src/{rest}")),
                    t,
                )
            })
            .collect();
        let got = hits(&files);
        assert!(got.is_empty(), "{}", got.join("\n"));
    }
}
