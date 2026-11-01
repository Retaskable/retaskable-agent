//! Which scouts run and which files each one reads (06.3-review.md, "Stage 2: scouts"). Pure:
//! Retaskable.com's `agent/src/scouts.ts` is the ground truth.
use super::diff::{DiffInfo, FileLines};

/// No security scout: in its first 61 runs it found nothing, so its checklist is part of
/// `Correctness`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoutKind {
    Correctness,
    Tests,
    Spec,
}

impl ScoutKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ScoutKind::Correctness => "correctness",
            ScoutKind::Tests => "tests",
            ScoutKind::Spec => "spec",
        }
    }
}

/// One scout to run, and the files it reads.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoutJob {
    pub scout: ScoutKind,
    /// e.g. "correctness 2/3", or just "correctness"/"tests"/"spec" when there's only one job of
    /// that kind.
    pub label: String,
    pub files: Vec<String>,
}

/// Packs `files` into groups of at most `limit` diff lines, biggest files first. A file bigger
/// than `limit` gets a group of its own.
pub fn pack_files(files: &[FileLines], limit: u32) -> Vec<Vec<String>> {
    struct Group {
        files: Vec<String>,
        lines: u32,
    }
    let mut sorted: Vec<&FileLines> = files.iter().collect();
    sorted.sort_by_key(|f| std::cmp::Reverse(f.lines));
    let mut groups: Vec<Group> = Vec::new();
    for f in sorted {
        match groups.iter_mut().find(|g| g.lines + f.lines <= limit) {
            Some(g) => {
                g.files.push(f.file.clone());
                g.lines += f.lines;
            }
            None => groups.push(Group {
                files: vec![f.file.clone()],
                lines: f.lines,
            }),
        }
    }
    groups
        .into_iter()
        .map(|mut g| {
            g.files.sort();
            g.files
        })
        .collect()
}

/// The scouts for a diff: none below `min_lines` (the reviewer reads a small diff itself), and
/// the `correctness` scout split by file above `max_lines`.
pub fn plan_scouts(diff: &DiffInfo, max_lines: u32, min_lines: u32) -> Vec<ScoutJob> {
    if diff.total_lines < min_lines {
        return Vec::new();
    }
    let all: Vec<String> = diff.files.iter().map(|f| f.file.clone()).collect();
    let groups: Vec<Vec<String>> = if diff.total_lines > max_lines {
        pack_files(&diff.files, max_lines)
    } else {
        vec![all.clone()]
    };
    let n = groups.len();
    let mut jobs = Vec::with_capacity(n + 2);
    for (i, files) in groups.into_iter().enumerate() {
        let label = if n > 1 {
            format!("correctness {}/{}", i + 1, n)
        } else {
            "correctness".to_string()
        };
        jobs.push(ScoutJob {
            scout: ScoutKind::Correctness,
            label,
            files,
        });
    }
    for (kind, label) in [(ScoutKind::Tests, "tests"), (ScoutKind::Spec, "spec")] {
        jobs.push(ScoutJob {
            scout: kind,
            label: label.to_string(),
            files: all.clone(),
        });
    }
    jobs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(file: &str, lines: u32) -> FileLines {
        FileLines {
            file: file.to_string(),
            lines,
        }
    }

    fn diff(files: Vec<FileLines>) -> DiffInfo {
        let total_lines = files.iter().map(|f| f.lines).sum();
        DiffInfo {
            stat: String::new(),
            files,
            backlog_changed: false,
            total_lines,
        }
    }

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn pack_files_fills_the_biggest_files_first_and_keeps_each_group_within_the_limit() {
        let groups = pack_files(&[file("a", 40), file("b", 60), file("c", 50), file("d", 50)], 100);
        assert_eq!(groups, vec![strings(&["a", "b"]), strings(&["c", "d"])]);
    }

    #[test]
    fn pack_files_gives_a_file_bigger_than_the_limit_a_group_of_its_own() {
        let groups = pack_files(&[file("huge", 500), file("x", 10), file("y", 10)], 100);
        assert_eq!(groups, vec![strings(&["huge"]), strings(&["x", "y"])]);
    }

    #[test]
    fn pack_files_sorts_each_groups_files_and_returns_nothing_for_no_files() {
        assert_eq!(
            pack_files(&[file("z", 1), file("m", 1)], 10),
            vec![strings(&["m", "z"])]
        );
        assert_eq!(pack_files(&[], 10), Vec::<Vec<String>>::new());
    }

    #[test]
    fn plan_scouts_runs_no_scouts_below_the_minimum() {
        assert_eq!(plan_scouts(&diff(vec![file("a", 5)]), 100, 6), vec![]);
        assert_eq!(plan_scouts(&diff(vec![file("a", 6)]), 100, 6).len(), 3);
    }

    #[test]
    fn plan_scouts_runs_one_scout_of_each_kind_over_every_file_when_the_diff_fits() {
        let jobs = plan_scouts(&diff(vec![file("a", 30), file("b", 20)]), 100, 0);
        let got: Vec<(&str, &str, Vec<String>)> = jobs
            .iter()
            .map(|j| (j.scout.as_str(), j.label.as_str(), j.files.clone()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("correctness", "correctness", strings(&["a", "b"])),
                ("tests", "tests", strings(&["a", "b"])),
                ("spec", "spec", strings(&["a", "b"])),
            ]
        );
    }

    #[test]
    fn plan_scouts_splits_only_the_correctness_scout_by_file_above_the_maximum() {
        let jobs = plan_scouts(&diff(vec![file("a", 80), file("b", 80), file("c", 10)]), 100, 0);
        let got: Vec<(&str, &str, Vec<String>)> = jobs
            .iter()
            .map(|j| (j.scout.as_str(), j.label.as_str(), j.files.clone()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("correctness", "correctness 1/2", strings(&["a", "c"])),
                ("correctness", "correctness 2/2", strings(&["b"])),
                ("tests", "tests", strings(&["a", "b", "c"])),
                ("spec", "spec", strings(&["a", "b", "c"])),
            ]
        );
    }

    #[test]
    fn plan_scouts_doesnt_split_a_diff_exactly_at_the_maximum() {
        let jobs = plan_scouts(&diff(vec![file("a", 60), file("b", 40)]), 100, 0);
        assert_eq!(jobs.iter().filter(|j| j.scout == ScoutKind::Correctness).count(), 1);
    }
}
