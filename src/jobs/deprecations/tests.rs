use serde_json::json;

use super::*;

struct Fake(Option<Vec<(String, String)>>);

impl SpecSource for Fake {
    fn read(&self) -> Option<Vec<(String, String)>> {
        self.0.clone()
    }
}

fn spec(name: &str, header: &str) -> (String, String) {
    (
        format!("docs/specs/{name}.md"),
        format!("# {name} — Spec\n\n{header}\n\n## One\n\nDeprecated: not here\n"),
    )
}

#[test]
fn specs_that_cannot_be_read_are_a_refusal() {
    assert_eq!(
        deprecations_job(&Fake(None)),
        Outcome::Refused("The default branch's specs can't be read yet.".to_string())
    );
}

#[test]
fn the_list_and_the_problems_are_answered_in_the_stores_shape() {
    let files = vec![
        spec(
            "mobile",
            "Deprecated: 2026-11-01. Remove after: 2026-12-01. Why: Replaced.",
        ),
        spec("plain", ""),
        spec("odd", "Deprecated: soon."),
        spec("bad-day", "Deprecated: 2026-02-30. Remove after: 2026-12-01."),
    ];
    let Outcome::Done { result, known } = deprecations_job(&Fake(Some(files))) else {
        panic!("expected a result");
    };
    assert!(known.is_empty());
    let v = serde_json::to_value(&result).unwrap();
    assert_eq!(
        v["deprecations"],
        json!([{
            "spec": "docs/specs/mobile.md", "name": "mobile", "title": "mobile",
            "since": "2026-11-01", "removeAfter": "2026-12-01", "why": "Replaced"
        }])
    );
    let problems = v["problems"].as_array().unwrap();
    assert_eq!(problems.len(), 2);
    assert_eq!(problems[0]["spec"], "docs/specs/odd.md");
    assert!(problems[1]["message"].as_str().unwrap().contains("isn't a date"));
}

#[test]
fn a_deprecation_with_no_why_leaves_it_out() {
    let files = vec![spec("a", "Deprecated: 2026-11-01. Remove after: 2026-12-01.")];
    let Outcome::Done { result, .. } = deprecations_job(&Fake(Some(files))) else {
        panic!()
    };
    let v = serde_json::to_value(&result).unwrap();
    assert!(v["deprecations"][0].get("why").is_none());
}

fn git(dir: &std::path::Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn git_specs_read_the_top_level_files_on_the_default_branch() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(dir.join("docs/specs/nested")).unwrap();
    std::fs::write(
        dir.join("docs/specs/a.md"),
        "# A — Spec\n\nDeprecated: 2026-11-01. Remove after: 2026-12-01.\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("docs/specs/nested/b.md"),
        "Deprecated: 2026-11-01. Remove after: 2026-12-01.\n",
    )
    .unwrap();
    std::fs::write(dir.join("docs/specs/notes.txt"), "x").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "specs"]);
    // The agent reads `origin/<branch>`: a ref of that name, as a fetch would leave it.
    git(dir, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    let src = GitSpecs {
        repo_path: dir.to_path_buf(),
        default_branch: Some("main".to_string()),
    };
    let files = src.read().unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].0, "docs/specs/a.md");
    let none = GitSpecs {
        repo_path: dir.to_path_buf(),
        default_branch: None,
    };
    assert_eq!(none.read(), None);
    let gone = GitSpecs {
        repo_path: dir.to_path_buf(),
        default_branch: Some("nope".to_string()),
    };
    assert_eq!(gone.read(), None);
}
