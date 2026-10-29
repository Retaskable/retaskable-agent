use super::*;
use crate::test_support::{git, repo_with_origin, run as run_git};

fn distinct(tag: &str, n: usize, from: usize) -> Vec<String> {
    (0..n)
        .map(|i| {
            format!(
                "  total_{tag} += weigh({tag}Items[{}], \"{tag} step {}\");",
                from + i,
                from + i
            )
        })
        .collect()
}

fn fn_lines(name: &str, body: &[String]) -> Vec<String> {
    let mut out = vec![format!("export function {name}() {{")];
    out.extend(body.iter().cloned());
    out.push("}".to_string());
    out
}

fn text_of(lines: &[String]) -> String {
    format!("{}\n", lines.join("\n"))
}

fn write_lines(repo: &Path, file: &str, lines: &[String]) {
    let path = repo.join(file);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, text_of(lines)).unwrap();
}

fn commit_all(repo: &Path, msg: &str) {
    run_git(git(repo).args(["add", "-A"]));
    run_git(git(repo).args(["commit", "--quiet", "--allow-empty", "-m", msg]));
}

fn passes(dir: &Path, gates: CodeGates) -> bool {
    run(dir, "main", &gates).unwrap().is_none()
}

/// A repo with `base` committed on `main` and pushed to `origin`, then `change` committed on
/// top locally (not pushed): the lane as a session leaves it. `None` in `change` deletes.
fn lane(root: &Path, base: &[(&str, Vec<String>)], change: &[(&str, Option<Vec<String>>)]) -> std::path::PathBuf {
    let repo = repo_with_origin(root);
    for (f, lines) in base {
        write_lines(&repo, f, lines);
    }
    commit_all(&repo, "base");
    run_git(git(&repo).args(["push", "--quiet", "origin", "main"]));
    for (f, lines) in change {
        match lines {
            Some(lines) => write_lines(&repo, f, lines),
            None => std::fs::remove_file(repo.join(f)).unwrap(),
        }
    }
    commit_all(&repo, "change");
    repo
}

#[test]
fn a_touched_source_file_over_the_limit_is_sent_back_with_where_it_could_split() {
    let big: Vec<String> = fn_lines("first", &distinct("a", 600, 0))
        .into_iter()
        .chain(fn_lines("second", &distinct("b", 600, 0)))
        .collect();
    let root = tempfile::tempdir().unwrap();
    let mut changed_big = big.clone();
    changed_big.push("// one more".to_string());
    let dir = lane(
        root.path(),
        &[
            ("src/big.ts", big),
            ("src/old.ts", distinct("c", 1200, 0)),
            ("docs/long.md", distinct("d", 1500, 0)),
        ],
        &[
            ("src/big.ts", Some(changed_big)),
            ("docs/long.md", Some(distinct("d", 1600, 0))),
        ],
    );
    let gates = CodeGates {
        max_file_lines: 1000,
        dup_lines: 0,
        max_spec_chars: 0,
    };
    let out = run(&dir, "main", &gates).unwrap().unwrap();
    assert!(out.message.contains("### Files over 1000 lines\n- `src/big.ts`: 1205 lines\n  - could split at line 603: `export function second() {`"));
    assert!(!out.message.contains("old.ts"));
    assert!(!out.message.contains("long.md"));
    assert!(!out.message.contains("Duplicated"));
    assert!(out.message.ends_with("Fix them, commit, and return the result again."));
    assert_eq!(out.note, "Code gates: 1 file over 1000 lines (src/big.ts)");

    // 0 turns it off, and every gate at 0 turns the check off entirely.
    assert!(passes(
        &dir,
        CodeGates {
            max_file_lines: 0,
            dup_lines: 0,
            max_spec_chars: 0
        }
    ));
    assert!(passes(
        &dir,
        CodeGates {
            max_file_lines: 2000,
            dup_lines: 0,
            max_spec_chars: 0
        }
    ));
}

#[test]
fn added_code_that_repeats_8_meaningful_lines_from_elsewhere_is_sent_back_with_both_ranges() {
    let shared = distinct("s", 10, 0);
    let root = tempfile::tempdir().unwrap();
    let mut copy = vec!["import { x } from './x';".to_string(), String::new()];
    copy.extend(fn_lines("copy", &shared));
    let dir = lane(
        root.path(),
        &[("src/a.ts", fn_lines("original", &shared))],
        &[("src/b.ts", Some(copy))],
    );
    let gates = CodeGates {
        max_file_lines: 1000,
        dup_lines: 8,
        max_spec_chars: 0,
    };
    let out = run(&dir, "main", &gates).unwrap().unwrap();
    // The ten copied lines are one range, not three overlapping windows.
    assert!(
        out.message
            .contains("### Duplicated code\n- `src/b.ts:4-13` repeats `src/a.ts:2-11`\n")
    );
    assert!(out.message.contains("Reuse the existing code instead of copying it"));
    assert_eq!(out.note, "Code gates: 1 copied block");
}

#[test]
fn seven_copied_lines_pass() {
    let root = tempfile::tempdir().unwrap();
    let dir = lane(
        root.path(),
        &[("src/a.ts", fn_lines("original", &distinct("s", 7, 0)))],
        &[("src/b.ts", Some(fn_lines("copy", &distinct("s", 7, 0))))],
    );
    assert!(run(&dir, "main", &CodeGates::default()).unwrap().is_none());
}

#[test]
fn code_moved_rather_than_copied_passes() {
    let root = tempfile::tempdir().unwrap();
    let dir = lane(
        root.path(),
        &[("src/a.ts", fn_lines("original", &distinct("s", 12, 0)))],
        &[
            ("src/a.ts", Some(vec!["export const a = 1;".to_string()])),
            ("src/b.ts", Some(fn_lines("moved", &distinct("s", 12, 0)))),
        ],
    );
    assert!(run(&dir, "main", &CodeGates::default()).unwrap().is_none());
}

#[test]
fn copies_in_tests_pass() {
    let root = tempfile::tempdir().unwrap();
    let dir = lane(
        root.path(),
        &[("src/a.test.ts", fn_lines("setup", &distinct("s", 12, 0)))],
        &[("src/b.test.ts", Some(fn_lines("setup", &distinct("s", 12, 0))))],
    );
    assert!(run(&dir, "main", &CodeGates::default()).unwrap().is_none());
}

#[test]
fn a_touched_spec_over_max_spec_chars_is_sent_back_with_the_headings_to_split_at() {
    let section = |n: usize| -> Vec<String> {
        let mut out = vec![format!("## {n}. Part {n}"), String::new()];
        out.extend(distinct(&format!("p{n}"), 40, 0));
        out
    };
    let mut big = vec![
        "# Big".to_string(),
        String::new(),
        "Status line.".to_string(),
        String::new(),
    ];
    for n in 1..=4 {
        big.extend(section(n));
    }
    let mut split_part = vec!["## 3. Part".to_string(), String::new()];
    for n in 1..=2 {
        split_part.push(format!("### 3.{n} Sub"));
        split_part.push(String::new());
        split_part.extend(distinct(&format!("s{n}"), 60, 0));
    }
    let root = tempfile::tempdir().unwrap();
    let dir = lane(
        root.path(),
        &[
            ("docs/specs/big.md", vec!["# Big".to_string()]),
            ("docs/specs/old.md", big.clone()),
        ],
        &[
            ("docs/specs/big.md", Some(big)),
            ("docs/specs/small.md", Some(vec!["# Small".to_string()])),
            ("docs/specs/split/03-part.md", Some(split_part)),
        ],
    );
    let gates = CodeGates {
        max_file_lines: 0,
        dup_lines: 0,
        max_spec_chars: 5000,
    };
    let out = run(&dir, "main", &gates).unwrap().unwrap();
    assert!(
        Regex::new(r"### Specs over 5000 characters\n- `docs/specs/big\.md`: \d+ characters\n  - could split at line \d+: `## \d\. Part \d`")
            .unwrap()
            .is_match(&out.message)
    );
    assert!(out.message.contains("- `docs/specs/split/03-part.md`: "));
    assert!(out.message.contains("could split at line 65: `### 3.2 Sub`"));
    assert!(out.message.contains("keep `<name>.md` as a short index"));
    assert!(!out.message.contains("old.md"));
    assert!(!out.message.contains("small.md"));
    assert!(
        Regex::new(
            r"^Code gates: 2 specs over 5000 characters \(docs/specs/big\.md, docs/specs/split/03-part\.md; split one"
        )
        .unwrap()
        .is_match(&out.note)
    );
    assert!(passes(
        &dir,
        CodeGates {
            max_file_lines: 0,
            dup_lines: 0,
            max_spec_chars: 0
        }
    ));
    assert!(passes(
        &dir,
        CodeGates {
            max_file_lines: 0,
            dup_lines: 0,
            max_spec_chars: 1_000_000
        }
    ));
}

#[test]
fn duplicates_ignores_noise_and_merges_a_block_repeated_twice() {
    let noise: Vec<String> = [
        "import a from 'a';",
        "import b from 'b';",
        "// a comment here",
        "}",
        "});",
        "",
        "  ]",
        "# heading",
        "/* block */",
        "* more",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let mut boiler = noise.clone();
    boiler.extend(noise.clone());
    let added = |file: &str, n: u32| vec![(file.to_string(), (1..=n).collect::<BTreeSet<u32>>())];
    let files = vec![
        ("a.ts".to_string(), text_of(&boiler)),
        ("b.ts".to_string(), text_of(&boiler)),
    ];
    assert_eq!(duplicates(&files, &added("b.ts", 20), 8), Vec::new());

    let block = distinct("t", 8, 0);
    let mut twice = block.clone();
    twice.push("const gap = 1;".to_string());
    twice.extend(block.clone());
    let files = vec![("n.ts".to_string(), text_of(&twice))];
    assert_eq!(
        duplicates(&files, &added("n.ts", 17), 8),
        vec![Copy {
            file: "n.ts".to_string(),
            from: 1,
            to: 8,
            other: OtherRange {
                file: "n.ts".to_string(),
                from: 10,
                to: 17
            },
        }]
    );
}

#[test]
fn added_lines_lists_each_files_added_line_numbers_a_deleted_file_isnt_there() {
    let diff = [
        "diff --git a/src/a.ts b/src/a.ts",
        "--- a/src/a.ts",
        "+++ b/src/a.ts",
        "@@ -3,0 +4,2 @@ x",
        "+one",
        "+two",
        "@@ -9 +11 @@",
        "-old",
        "+new",
        "@@ -20,2 +22,0 @@",
        "-gone",
        "-gone",
        "diff --git a/old.ts b/old.ts",
        "--- a/old.ts",
        "+++ /dev/null",
        "@@ -1,2 +0,0 @@",
        "diff --git a/my file.ts b/my file.ts",
        "+++ b/my file.ts",
        "@@ -0,0 +1 @@",
    ]
    .join("\n");
    let got: Vec<(String, Vec<u32>)> = added_lines(&diff)
        .into_iter()
        .map(|(f, s)| (f, s.into_iter().collect()))
        .collect();
    assert_eq!(
        got,
        vec![
            ("src/a.ts".to_string(), vec![4, 5, 11]),
            ("my file.ts".to_string(), vec![1])
        ]
    );
}

#[test]
fn oversized_split_points_are_top_level_declarations_near_equal_parts_at_most_3_across_files() {
    let file = |name: &str, n: usize| {
        let text: String = (0..n)
            .map(|i| {
                if i % 100 == 0 {
                    format!("export function f{i}() {{")
                } else {
                    "  x();".to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        (name.to_string(), text)
    };
    let out = oversized(&[file("a.ts", 2000), file("b.ts", 1100), file("c.ts", 500)], 1000);
    let simplified: Vec<(String, usize, Vec<u32>)> = out
        .iter()
        .map(|b| (b.file.clone(), b.lines, b.splits.iter().map(|s| s.line).collect()))
        .collect();
    assert_eq!(
        simplified,
        vec![
            ("a.ts".to_string(), 2000, vec![701, 1301]),
            ("b.ts".to_string(), 1100, vec![501])
        ]
    );
    assert_eq!(oversized(&[file("a.ts", 9000)], 1000)[0].splits.len(), 3);
}

#[test]
fn is_source_and_is_test_tell_code_generated_vendored_code_and_tests_apart() {
    for f in ["src/a.ts", "app/x.tsx", "lib/m.rs", "pkg/a.go", "a.py", "s.sh"] {
        assert!(is_source(f), "{f}");
    }
    for f in [
        "README.md",
        "package.json",
        "Cargo.lock",
        "node_modules/x/a.js",
        "dist/a.js",
        "a.min.js",
        "types.d.ts",
        "api.generated.ts",
        "vendor/x.go",
    ] {
        assert!(!is_source(f), "{f}");
    }
    for f in [
        "src/a.test.ts",
        "src/a.spec.tsx",
        "test/x.ts",
        "pkg/a_test.go",
        "tests/test_x.py",
        "x/__tests__/y.js",
    ] {
        assert!(is_test(f), "{f}");
    }
    for f in ["src/attest.ts", "src/contest.ts", "latest/x.ts"] {
        assert!(!is_test(f), "{f}");
    }
}
