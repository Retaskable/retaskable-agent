//! Whether a repo's CI will run for a pull request into its base branch (docs/specs/agent/09-web-ui.md
//! section 9.1, "The scan"; Retaskable.com's `team-branch.md` section 3.2), ported from `ciCoverage` and
//! `readOn` in `agent/src/ciCoverage.ts`. Only the GitHub-workflows half: this agent doesn't yet support
//! other code hosts, so Woodpecker and GitLab CI (which `ciCoverage.ts` also answers `covered` for) aren't
//! ported.
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::sync::LazyLock;

use regex::Regex;

const PR_EVENTS: [&str; 2] = ["pull_request", "pull_request_target"];

/// A `.github/workflows` file read from a git ref.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowFile {
    pub file: String,
    /// `None` when it couldn't be read.
    pub text: Option<String>,
}

/// `CiCoverage["answer"]` in `shared/types.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CiAnswer {
    /// No `.github/workflows` on the base branch (and, in the TypeScript original, no Woodpecker
    /// pipeline or GitLab CI file either).
    #[default]
    None,
    Covered,
    NotCovered,
    Unknown,
}

/// A workflow found while answering `NotCovered`, for the warning (`why` in `ciCoverage.ts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CiWorkflow {
    pub file: String,
    /// `None` when the workflow has no `pull_request`/`pull_request_target` trigger at all.
    pub branches: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CiCoverage {
    pub answer: CiAnswer,
    /// Only set with `NotCovered`.
    pub workflows: Vec<CiWorkflow>,
}

#[derive(Debug, Clone, Default)]
struct Trigger {
    branches: Option<Vec<String>>,
    branches_ignore: Option<Vec<String>>,
    paths: bool,
}

/// team-branch.md section 3.2's table: `covered` / `not_covered` / `none` / `unknown`, over workflow
/// files already read from the base branch (`read_workflows`).
pub fn ci_coverage(workflows: &[WorkflowFile], base_branch: &str) -> CiCoverage {
    if workflows.is_empty() {
        return CiCoverage::default();
    }
    let mut unknown = false;
    let mut found = Vec::new();
    for w in workflows {
        let events = match &w.text {
            None => {
                unknown = true;
                continue;
            }
            Some(text) => match read_on(text) {
                Ok(events) => events,
                Err(Unreadable) => {
                    unknown = true;
                    continue;
                }
            },
        };
        let prs: Vec<&Trigger> = PR_EVENTS.iter().filter_map(|e| events.get(*e)).collect();
        let mut covered = false;
        for t in &prs {
            if !branch_runs(t, base_branch) {
                continue;
            }
            if t.paths {
                unknown = true;
            } else {
                covered = true;
                break;
            }
        }
        if covered {
            return CiCoverage {
                answer: CiAnswer::Covered,
                workflows: Vec::new(),
            };
        }
        let branches = if prs.is_empty() {
            None
        } else {
            Some(
                prs.iter()
                    .flat_map(|t| t.branches.clone().unwrap_or_default())
                    .collect(),
            )
        };
        found.push(CiWorkflow {
            file: w.file.clone(),
            branches,
        });
    }
    if unknown {
        CiCoverage {
            answer: CiAnswer::Unknown,
            workflows: Vec::new(),
        }
    } else {
        CiCoverage {
            answer: CiAnswer::NotCovered,
            workflows: found,
        }
    }
}

/// The workflow files (`*.yml`/`*.yaml` under `.github/workflows`) on `git_ref`, read with `git
/// ls-tree`/`git show`; empty when the ref or the directory doesn't exist (not an error: `none` is
/// a valid answer).
pub fn read_workflows(repo: &Path, git_ref: &str) -> Vec<WorkflowFile> {
    let ls = Command::new("git")
        .current_dir(repo)
        .args(["ls-tree", "--name-only", &format!("{git_ref}:.github/workflows")])
        .output();
    let Ok(ls) = ls else {
        return Vec::new();
    };
    if !ls.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&ls.stdout)
        .lines()
        .map(str::trim)
        .filter(|f| f.ends_with(".yml") || f.ends_with(".yaml"))
        .map(|file| {
            let show = Command::new("git")
                .current_dir(repo)
                .args(["show", &format!("{git_ref}:.github/workflows/{file}")])
                .output();
            let text = match show {
                Ok(out) if out.status.success() => Some(String::from_utf8_lossy(&out.stdout).into_owned()),
                _ => None,
            };
            WorkflowFile {
                file: file.to_string(),
                text,
            }
        })
        .collect()
}

/// Setup's warning (team-branch.md section 3.4): `None` unless `cov` is `NotCovered`.
pub fn ci_warning(cov: &CiCoverage, base_branch: &str) -> Option<String> {
    if cov.answer != CiAnswer::NotCovered {
        return None;
    }
    let add = if base_branch.starts_with("dev/") {
        "'dev/**'".to_string()
    } else {
        format!("'{base_branch}'")
    };
    Some(format!(
        "CI won't run for PRs into `{base_branch}`: {}. Add `{add}` to its `pull_request` branches so slices get CI; until then they're checked by this project's own checks only.",
        why(cov, base_branch)
    ))
}

fn why(cov: &CiCoverage, base_branch: &str) -> String {
    cov.workflows
        .iter()
        .map(|w| match &w.branches {
            None => format!("`{}` doesn't run on pull requests", w.file),
            Some(branches) => {
                let into: Vec<&str> = branches
                    .iter()
                    .filter(|b| !b.starts_with('!'))
                    .map(String::as_str)
                    .collect();
                if into.is_empty() {
                    format!("`{}` skips pull requests into `{}`", w.file, base_branch)
                } else {
                    let list = into.iter().map(|b| format!("`{b}`")).collect::<Vec<_>>().join(", ");
                    format!("`{}` runs on pull requests into {} only", w.file, list)
                }
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// A trigger's `branches`/`branches-ignore` filter against `branch`, following GitHub's filter
/// patterns (`*`, `**`, `?`, `!` negation, in order).
fn branch_runs(t: &Trigger, branch: &str) -> bool {
    if let Some(branches) = &t.branches {
        let mut runs = false;
        for p in branches {
            let (neg, pat) = match p.strip_prefix('!') {
                Some(rest) => (true, rest),
                None => (false, p.as_str()),
            };
            if glob_match(pat, branch) {
                runs = !neg;
            }
        }
        return runs;
    }
    if let Some(ignore) = &t.branches_ignore {
        return !ignore.iter().any(|p| glob_match(p, branch));
    }
    true
}

/// GitHub's filter patterns: `*` within a path segment, `**` across them, `?` / `+` on the character
/// before, `[...]`.
fn glob_match(pattern: &str, s: &str) -> bool {
    let chars: Vec<char> = pattern.chars().collect();
    let mut re = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '*' {
            if chars.get(i + 1) == Some(&'*') {
                re.push_str(".*");
                i += 1;
            } else {
                re.push_str("[^/]*");
            }
        } else if c == '?' || c == '+' {
            re.push(c);
        } else if c == '[' {
            match chars[i..].iter().position(|&ch| ch == ']') {
                Some(offset) => {
                    let end = i + offset;
                    re.extend(&chars[i..=end]);
                    i = end;
                }
                None => re.push_str("\\["),
            }
        } else {
            if ".^$|(){}\\]".contains(c) {
                re.push('\\');
            }
            re.push(c);
        }
        i += 1;
    }
    match Regex::new(&format!("^{re}$")) {
        Ok(re) => re.is_match(s),
        Err(_) => pattern == s,
    }
}

// ── The `on:` block ──────────────────────────────────────────────────────────

#[derive(Debug)]
struct Unreadable;

#[derive(Debug, Clone)]
struct Line {
    indent: usize,
    text: String,
}

static COMMENT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(^|\s)#.*$").unwrap());
static TOP_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"^(on|"on"|'on'|true):(\s|$)"#).unwrap());
static ENTRY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^("[^"]*"|'[^']*'|[^:\s][^:]*?):(?:\s+(.*))?$"#).unwrap());

fn lines(text: &str) -> Result<Vec<Line>, Unreadable> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let no_comment = COMMENT_RE.replace(raw, "");
        let trimmed_end = no_comment.trim_end();
        if trimmed_end.trim().is_empty() {
            continue;
        }
        if has_tab_in_leading_ws(trimmed_end) {
            return Err(Unreadable);
        }
        let stripped = trimmed_end.trim_start();
        let indent = trimmed_end.len() - stripped.len();
        out.push(Line {
            indent,
            text: stripped.to_string(),
        });
    }
    Ok(out)
}

fn has_tab_in_leading_ws(s: &str) -> bool {
    for c in s.chars() {
        if c == '\t' {
            return true;
        }
        if c != ' ' {
            break;
        }
    }
    false
}

fn unquote(s: &str) -> String {
    let t = s.trim();
    let mut chars = t.chars();
    match (chars.next(), chars.last()) {
        (Some(first), Some(last)) if (first == '\'' || first == '"') && first == last && t.len() > 1 => {
            t[1..t.len() - 1].to_string()
        }
        _ => t.to_string(),
    }
}

fn flow_list(s: &str) -> Result<Vec<String>, Unreadable> {
    let inner = s.trim();
    if !inner.starts_with('[') || !inner.ends_with(']') {
        return Err(Unreadable);
    }
    let body = &inner[1..inner.len() - 1];
    Ok(body.split(',').map(unquote).filter(|x| !x.is_empty()).collect())
}

/// A key's value: inline (a scalar or a flow list) or the block under it.
fn list_value(inline: &str, block: &[Line]) -> Result<Vec<String>, Unreadable> {
    if !inline.is_empty() {
        return if inline.starts_with('[') {
            flow_list(inline)
        } else {
            Ok(vec![unquote(inline)])
        };
    }
    block
        .iter()
        .map(|l| l.text.strip_prefix("- ").map(unquote).ok_or(Unreadable))
        .collect()
}

struct Entry {
    key: String,
    inline: String,
    under: Vec<Line>,
}

/// Splits lines at one indent into `key: value` entries, each with the deeper lines under it.
fn entries(block: &[Line]) -> Result<Vec<Entry>, Unreadable> {
    if block.is_empty() {
        return Ok(Vec::new());
    }
    let at = block[0].indent;
    let mut out: Vec<Entry> = Vec::new();
    for l in block {
        if l.indent > at {
            out.last_mut().ok_or(Unreadable)?.under.push(l.clone());
            continue;
        }
        if l.indent < at {
            return Err(Unreadable);
        }
        let caps = ENTRY_RE.captures(&l.text).ok_or(Unreadable)?;
        let key = unquote(&caps[1]);
        let inline = caps.get(2).map(|m| m.as_str()).unwrap_or("").trim().to_string();
        out.push(Entry {
            key,
            inline,
            under: Vec::new(),
        });
    }
    Ok(out)
}

/// The workflow's events and their branch filters. `Err` when it can't follow the file.
fn read_on(text: &str) -> Result<HashMap<String, Trigger>, Unreadable> {
    let all = lines(text)?;
    let mut events = HashMap::new();
    let Some(top) = all.iter().position(|l| l.indent == 0 && TOP_RE.is_match(&l.text)) else {
        return Ok(events);
    };
    let colon = all[top].text.find(':').ok_or(Unreadable)?;
    let inline = all[top].text[colon + 1..].trim().to_string();
    let mut block = Vec::new();
    let mut i = top + 1;
    while i < all.len() && all[i].indent > 0 {
        block.push(all[i].clone());
        i += 1;
    }
    if !inline.is_empty() {
        if inline.starts_with('{') || inline.starts_with('&') || inline.starts_with('*') {
            return Err(Unreadable);
        }
        for e in list_value(&inline, &[])? {
            events.insert(e, Trigger::default());
        }
        return Ok(events);
    }
    if block.first().is_some_and(|l| l.text.starts_with("- ")) {
        for e in list_value("", &block)? {
            events.insert(e, Trigger::default());
        }
        return Ok(events);
    }
    for e in entries(&block)? {
        if !e.inline.is_empty() && e.inline != "null" && e.inline != "~" && e.inline != "{}" {
            return Err(Unreadable);
        }
        let mut t = Trigger::default();
        for f in entries(&e.under)? {
            match f.key.as_str() {
                "branches" => t.branches = Some(list_value(&f.inline, &f.under)?),
                "branches-ignore" => t.branches_ignore = Some(list_value(&f.inline, &f.under)?),
                "paths" | "paths-ignore" => t.paths = true,
                _ => {}
            }
        }
        events.insert(e.key, t);
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wf(file: &str, text: &str) -> WorkflowFile {
        WorkflowFile {
            file: file.to_string(),
            text: Some(text.to_string()),
        }
    }

    #[test]
    fn no_workflows_is_none() {
        assert_eq!(ci_coverage(&[], "main").answer, CiAnswer::None);
    }

    #[test]
    fn a_pull_request_trigger_with_no_branches_filter_is_covered() {
        let w = wf("ci.yml", "on:\n  pull_request:\n  push:\n");
        assert_eq!(ci_coverage(&[w], "main").answer, CiAnswer::Covered);
    }

    #[test]
    fn a_matching_branches_filter_is_covered() {
        let w = wf("ci.yml", "on:\n  pull_request:\n    branches: [main, 'dev/**']\n");
        assert_eq!(ci_coverage(std::slice::from_ref(&w), "main").answer, CiAnswer::Covered);
        assert_eq!(ci_coverage(&[w], "dev/jamez667").answer, CiAnswer::Covered);
    }

    #[test]
    fn a_non_matching_branches_filter_is_not_covered_with_the_workflow_named() {
        let w = wf("ci.yml", "on:\n  pull_request:\n    branches: [main]\n");
        let cov = ci_coverage(&[w], "dev/jamez667");
        assert_eq!(cov.answer, CiAnswer::NotCovered);
        assert_eq!(
            cov.workflows,
            vec![CiWorkflow {
                file: "ci.yml".to_string(),
                branches: Some(vec!["main".to_string()]),
            }]
        );
    }

    #[test]
    fn a_push_only_workflow_is_not_covered_with_no_branches() {
        let w = wf("ci.yml", "on:\n  push:\n    branches: [main]\n");
        let cov = ci_coverage(&[w], "main");
        assert_eq!(cov.answer, CiAnswer::NotCovered);
        assert_eq!(
            cov.workflows,
            vec![CiWorkflow {
                file: "ci.yml".to_string(),
                branches: None
            }]
        );
    }

    #[test]
    fn a_short_form_on_list_with_no_pull_request_is_not_covered() {
        let w = wf("ci.yml", "on: [push, workflow_dispatch]\n");
        assert_eq!(ci_coverage(&[w], "main").answer, CiAnswer::NotCovered);
    }

    #[test]
    fn negation_follows_order() {
        let w = wf(
            "ci.yml",
            "on:\n  pull_request:\n    branches: ['dev/**', '!dev/scratch']\n",
        );
        assert_eq!(
            ci_coverage(std::slice::from_ref(&w), "dev/jamez667").answer,
            CiAnswer::Covered
        );
        assert_eq!(ci_coverage(&[w], "dev/scratch").answer, CiAnswer::NotCovered);
    }

    #[test]
    fn a_paths_filter_is_unknown_when_it_would_otherwise_run() {
        let w = wf(
            "ci.yml",
            "on:\n  pull_request:\n    branches: [main]\n    paths: ['src/**']\n",
        );
        assert_eq!(ci_coverage(&[w], "main").answer, CiAnswer::Unknown);
    }

    #[test]
    fn an_unreadable_workflow_is_unknown() {
        let w = wf("ci.yml", "on:\n\tpull_request:\n");
        assert_eq!(ci_coverage(&[w], "main").answer, CiAnswer::Unknown);
    }

    #[test]
    fn a_workflow_with_no_text_is_unknown() {
        let w = WorkflowFile {
            file: "ci.yml".to_string(),
            text: None,
        };
        assert_eq!(ci_coverage(&[w], "main").answer, CiAnswer::Unknown);
    }

    #[test]
    fn pull_request_target_counts_too() {
        let w = wf("ci.yml", "on:\n  pull_request_target:\n    branches: [main]\n");
        assert_eq!(ci_coverage(&[w], "main").answer, CiAnswer::Covered);
    }

    #[test]
    fn branches_ignore_excludes_a_match() {
        let w = wf("ci.yml", "on:\n  pull_request:\n    branches-ignore: [main]\n");
        let cov = ci_coverage(std::slice::from_ref(&w), "main");
        assert_eq!(cov.answer, CiAnswer::NotCovered);
        assert_eq!(ci_coverage(&[w], "dev/x").answer, CiAnswer::Covered);
    }

    #[test]
    fn ci_warning_text_names_the_workflow_and_its_branches() {
        let w = wf("ci.yml", "on:\n  pull_request:\n    branches: [main]\n");
        let cov = ci_coverage(&[w], "dev/jamez667");
        assert_eq!(
            ci_warning(&cov, "dev/jamez667").unwrap(),
            "CI won't run for PRs into `dev/jamez667`: `ci.yml` runs on pull requests into `main` only. Add `'dev/**'` to its `pull_request` branches so slices get CI; until then they're checked by this project's own checks only."
        );
    }

    #[test]
    fn ci_warning_is_none_unless_not_covered() {
        assert_eq!(ci_warning(&CiCoverage::default(), "main"), None);
        let covered = CiCoverage {
            answer: CiAnswer::Covered,
            workflows: Vec::new(),
        };
        assert_eq!(ci_warning(&covered, "main"), None);
    }

    #[test]
    fn glob_match_handles_double_star_and_negation() {
        assert!(glob_match("dev/**", "dev/jamez667"));
        assert!(!glob_match("dev/*", "dev/a/b"));
        assert!(glob_match("main", "main"));
        assert!(!glob_match("main", "develop"));
    }

    #[test]
    fn read_on_reads_a_flow_list_shorthand() {
        let events = read_on("on: [push, pull_request]\n").unwrap();
        assert!(events.contains_key("pull_request"));
        assert!(events.contains_key("push"));
    }

    #[test]
    fn read_on_reads_a_dash_list_block() {
        let events = read_on("on:\n  - push\n  - pull_request\n").unwrap();
        assert!(events.contains_key("pull_request"));
    }

    #[test]
    fn read_on_is_empty_without_an_on_key() {
        assert_eq!(
            read_on("name: CI\njobs:\n  build:\n    runs-on: ubuntu-latest\n")
                .unwrap()
                .len(),
            0
        );
    }

    #[test]
    fn read_on_strips_trailing_comments() {
        let events = read_on("on:\n  pull_request: # trigger\n    branches: [main] # only main\n").unwrap();
        assert_eq!(events["pull_request"].branches, Some(vec!["main".to_string()]));
    }
}
