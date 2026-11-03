//! What a `watch` run decides from a change's checks (docs/specs/pieces.md section 3.2, `watch` row; the CI-reading
//! rules of 05-item-lifecycle.md's `awaiting_ci` row). Pure code over the plain output of `gh pr checks <pr>`
//! (so older `gh` versions without `--json` work) and, where `gh` has it, `gh pr checks --json`, which tells
//! pending checks that never started from those that started but never finished. The `gh` program is a parameter
//! so tests can use a fake.
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, bail};
use serde::Deserialize;

use crate::ci_coverage::CiAnswer;
use crate::process_tree::ProcessTree;

/// Five `gh` errors in a row end the watch as `failed`.
pub const MAX_GH_ERRORS: u32 = 5;
/// A `gh` error is retried after this long.
pub const GH_RETRY: Duration = Duration::from_secs(60);
/// A failing check's log is cut to this many trailing lines.
pub const LOG_TAIL_LINES: usize = 120;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckState {
    Pass,
    Fail,
    Pending,
    Skipping,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub name: String,
    pub state: CheckState,
    pub link: String,
    /// From `--json` only: whether the check ever started. `None` when unknown.
    pub started: Option<bool>,
}

fn state_of(word: &str) -> Option<CheckState> {
    Some(match word.trim().to_ascii_lowercase().as_str() {
        "pass" => CheckState::Pass,
        "fail" => CheckState::Fail,
        "pending" => CheckState::Pending,
        "skipping" => CheckState::Skipping,
        "cancel" => CheckState::Cancel,
        _ => return None,
    })
}

/// `gh pr checks <pr>`'s plain output: one check per line, `name  state  elapsed  link  description`, tab
/// separated. Lines that aren't a check are ignored.
pub fn parse_plain(out: &str) -> Vec<Check> {
    out.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            let state = state_of(f.get(1)?)?;
            let link = f.iter().skip(2).find(|s| s.starts_with("http")).unwrap_or(&"");
            Some(Check {
                name: f[0].trim().to_string(),
                state,
                link: link.trim().to_string(),
                started: None,
            })
        })
        .collect()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JsonCheck {
    name: String,
    #[serde(default)]
    bucket: String,
    #[serde(default)]
    link: String,
    #[serde(default)]
    started_at: Option<String>,
}

/// `gh pr checks --json name,bucket,link,startedAt`. A check whose `startedAt` is empty or the zero time never
/// started.
pub fn parse_json(out: &str) -> Result<Vec<Check>> {
    let rows: Vec<JsonCheck> = serde_json::from_str(out.trim())?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            let started = r
                .started_at
                .as_deref()
                .is_some_and(|s| !s.is_empty() && !s.starts_with("0001-"));
            Some(Check {
                name: r.name,
                state: state_of(&r.bucket)?,
                link: r.link,
                started: Some(started),
            })
        })
        .collect())
}

/// The plain list, with each check's `started` filled in from the `--json` list by name.
pub fn with_started(mut plain: Vec<Check>, json: &[Check]) -> Vec<Check> {
    for c in &mut plain {
        c.started = json.iter().find(|j| j.name == c.name).and_then(|j| j.started);
    }
    plain
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failing {
    pub name: String,
    pub link: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Read again later.
    Wait,
    /// Every check passed (or was skipped), or none ran; `note` says why when there was no CI.
    Green {
        note: Option<String>,
    },
    Red {
        failing: Vec<Failing>,
    },
    /// CI stayed pending past `ciMaxMinutes`.
    Failed {
        why: String,
    },
}

fn names(list: &[&Check]) -> String {
    list.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(", ")
}

/// Decides from the checks read. `elapsed` is how long the watch has been going; `grace_minutes` and
/// `max_minutes` are the project's `ciGraceMinutes` and `ciMaxMinutes`.
pub fn decide(
    checks: &[Check],
    coverage: CiAnswer,
    elapsed: Duration,
    grace_minutes: u32,
    max_minutes: u32,
) -> Decision {
    if matches!(coverage, CiAnswer::NotCovered | CiAnswer::None) {
        return Decision::Green {
            note: Some("no CI runs for this base branch".into()),
        };
    }
    let live: Vec<&Check> = checks.iter().filter(|c| c.state != CheckState::Skipping).collect();
    if live.is_empty() {
        if !checks.is_empty() {
            return Decision::Green { note: None };
        }
        if elapsed >= Duration::from_secs(u64::from(grace_minutes) * 60) {
            return Decision::Green {
                note: Some("no CI checks ran".into()),
            };
        }
        return Decision::Wait;
    }
    let failing: Vec<Failing> = live
        .iter()
        .filter(|c| matches!(c.state, CheckState::Fail | CheckState::Cancel))
        .map(|c| Failing {
            name: c.name.clone(),
            link: c.link.clone(),
        })
        .collect();
    if !failing.is_empty() {
        return Decision::Red { failing };
    }
    let pending: Vec<&Check> = live
        .iter()
        .copied()
        .filter(|c| c.state == CheckState::Pending)
        .collect();
    if pending.is_empty() {
        return Decision::Green { note: None };
    }
    if elapsed < Duration::from_secs(u64::from(max_minutes) * 60) {
        return Decision::Wait;
    }
    let never_started: Vec<&Check> = pending.iter().copied().filter(|c| c.started == Some(false)).collect();
    let never_finished: Vec<&Check> = pending.iter().copied().filter(|c| c.started != Some(false)).collect();
    let mut why = format!("CI was still pending after the limit of {max_minutes} minutes (ciMaxMinutes).");
    if !never_started.is_empty() {
        why.push_str(&format!(
            " Never started (queued with no runner): {}.",
            names(&never_started)
        ));
    }
    if !never_finished.is_empty() {
        why.push_str(&format!(" Started but never finished: {}.", names(&never_finished)));
    }
    Decision::Failed { why }
}

/// Counts `gh` errors in a row (no answer from the code host, not a failed check).
#[derive(Debug, Default)]
pub struct GhErrors(u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterError {
    /// Try again after this long.
    Retry(Duration),
    /// Five in a row.
    Failed,
}

impl GhErrors {
    pub fn error(&mut self) -> AfterError {
        self.0 += 1;
        if self.0 >= MAX_GH_ERRORS {
            AfterError::Failed
        } else {
            AfterError::Retry(GH_RETRY)
        }
    }

    /// Any answer, even a failing check, starts the count again.
    pub fn answered(&mut self) {
        self.0 = 0;
    }
}

/// Runs `gh <args>`; stdout and whether it exited 0. `gh pr checks` exits non-zero for failing or pending
/// checks while still printing them, so only a start failure or an empty stdout with an error is no answer.
fn gh_out(gh: &Path, dir: &Path, args: &[&str], reg: Reg) -> Result<(String, String, bool)> {
    let mut command = Command::new(gh);
    command
        .current_dir(dir)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    ProcessTree::prepare_sync(&mut command);
    let child = command.spawn()?;
    let tree = Arc::new(ProcessTree::join_sync(&child)?);
    reg(tree.clone());
    let out = child.wait_with_output()?;
    drop(tree);
    Ok((
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    ))
}

/// Where each `gh` started is handed as a process tree, so a stop can cut it.
pub type Reg<'a> = &'a dyn Fn(Arc<ProcessTree>);

/// Reads a change's checks. `Err` is a `gh` error (no answer); a change with no checks is `Ok(vec![])`. The
/// `--json` read is best effort, for `started`.
pub fn read_checks(gh: &Path, dir: &Path, pr: &str, reg: Reg) -> Result<Vec<Check>> {
    let (out, err, ok) = gh_out(gh, dir, &["pr", "checks", pr], reg)?;
    let plain = parse_plain(&out);
    if plain.is_empty() && !ok && !err.contains("no checks reported") {
        bail!("gh pr checks failed: {}", err.trim());
    }
    if !plain.iter().any(|c| c.state == CheckState::Pending) {
        return Ok(plain);
    }
    let json = match gh_out(
        gh,
        dir,
        &["pr", "checks", pr, "--json", "name,bucket,link,startedAt"],
        reg,
    ) {
        Ok((out, _, _)) => parse_json(&out).unwrap_or_default(),
        Err(_) => Vec::new(),
    };
    Ok(with_started(plain, &json))
}

/// A failing check as the `red` result carries it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedEntry {
    pub name: String,
    pub link: String,
    pub log: String,
}

fn run_id(link: &str) -> Option<&str> {
    let rest = link.split("/runs/").nth(1)?;
    let id = rest.split(['/', '?', '#']).next()?;
    (!id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())).then_some(id)
}

/// The last `LOG_TAIL_LINES` lines of a check's `gh run view <id> --log-failed`; empty when the link has no run
/// or `gh` can't say.
pub fn red_entry(gh: &Path, dir: &Path, check: &Failing, reg: Reg) -> RedEntry {
    let log = run_id(&check.link)
        .and_then(|id| gh_out(gh, dir, &["run", "view", id, "--log-failed"], reg).ok())
        .map(|(out, _, _)| {
            let lines: Vec<&str> = out.lines().collect();
            lines[lines.len().saturating_sub(LOG_TAIL_LINES)..].join("\n")
        })
        .unwrap_or_default();
    RedEntry {
        name: check.name.clone(),
        link: check.link.clone(),
        log,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: u64 = 60;
    fn at(m: u64) -> Duration {
        Duration::from_secs(m * MIN)
    }
    fn chk(name: &str, state: CheckState, started: Option<bool>) -> Check {
        Check {
            name: name.into(),
            state,
            link: format!("https://x/{name}"),
            started,
        }
    }
    fn decide_at(checks: &[Check], m: u64) -> Decision {
        decide(checks, CiAnswer::Covered, at(m), 10, 120)
    }

    #[test]
    fn plain_output_is_read_tab_separated() {
        let out = "build\tpass\t1m2s\thttps://github.com/o/r/actions/runs/11/job/1\t\n\
                   test (ubuntu)\tfail\t3m\thttps://github.com/o/r/actions/runs/12/job/2\tboom\n\
                   lint\tpending\t0\thttps://github.com/o/r/actions/runs/13/job/3\t\n\
                   docs\tskipping\t0\thttps://x/docs\t\n\
                   e2e\tcancel\t5s\thttps://x/e2e\t\n";
        let got = parse_plain(out);
        let states: Vec<_> = got.iter().map(|c| (c.name.as_str(), c.state)).collect();
        assert_eq!(
            states,
            [
                ("build", CheckState::Pass),
                ("test (ubuntu)", CheckState::Fail),
                ("lint", CheckState::Pending),
                ("docs", CheckState::Skipping),
                ("e2e", CheckState::Cancel)
            ]
        );
        assert_eq!(got[1].link, "https://github.com/o/r/actions/runs/12/job/2");
        assert!(parse_plain("").is_empty());
        assert!(parse_plain("no checks reported on the 'x' branch\n").is_empty());
    }

    #[test]
    fn json_output_says_which_checks_never_started() {
        let out = r#"[{"name":"a","bucket":"pending","link":"https://x/a","startedAt":"0001-01-01T00:00:00Z"},
            {"name":"b","bucket":"pending","link":"https://x/b","startedAt":"2026-11-03T10:00:00Z"},
            {"name":"c","bucket":"pass","link":"","startedAt":null}]"#;
        let got = parse_json(out).unwrap();
        assert_eq!(
            got.iter().map(|c| c.started).collect::<Vec<_>>(),
            [Some(false), Some(true), Some(false)]
        );
        assert!(parse_json("[]").unwrap().is_empty());
        let merged = with_started(parse_plain("a\tpending\t0\thttps://x/a\t\n"), &got);
        assert_eq!(merged[0].started, Some(false));
    }

    #[test]
    fn pass_and_skipped_are_green_and_fail_is_red() {
        let pass = [chk("a", CheckState::Pass, None), chk("b", CheckState::Skipping, None)];
        assert_eq!(decide_at(&pass, 1), Decision::Green { note: None });
        let all_skipped = [chk("a", CheckState::Skipping, None)];
        assert_eq!(decide_at(&all_skipped, 1), Decision::Green { note: None });
        let red = [
            chk("a", CheckState::Fail, None),
            chk("b", CheckState::Pending, None),
            chk("c", CheckState::Pass, None),
        ];
        assert_eq!(
            decide_at(&red, 1),
            Decision::Red {
                failing: vec![Failing {
                    name: "a".into(),
                    link: "https://x/a".into()
                }]
            }
        );
    }

    #[test]
    fn no_checks_wait_for_the_grace_then_read_as_green_with_a_note() {
        assert_eq!(decide_at(&[], 9), Decision::Wait);
        assert!(matches!(decide_at(&[], 10), Decision::Green { note: Some(n) } if n.contains("no CI")));
    }

    #[test]
    fn ci_that_cannot_run_for_the_base_is_green_at_once() {
        for answer in [CiAnswer::NotCovered, CiAnswer::None] {
            let d = decide(&[], answer, at(0), 10, 120);
            assert!(matches!(d, Decision::Green { note: Some(_) }));
        }
    }

    #[test]
    fn pending_waits_until_the_limit_then_names_what_never_started_or_finished() {
        let pending = [
            chk("queued", CheckState::Pending, Some(false)),
            chk("slow", CheckState::Pending, Some(true)),
            chk("ok", CheckState::Pass, None),
        ];
        assert_eq!(decide_at(&pending, 119), Decision::Wait);
        let Decision::Failed { why } = decide_at(&pending, 120) else {
            panic!("not failed")
        };
        assert!(why.contains("120 minutes"), "{why}");
        assert!(why.contains("Never started (queued with no runner): queued."), "{why}");
        assert!(why.contains("Started but never finished: slow."), "{why}");
        let d = decide(&pending, CiAnswer::Covered, at(31), 10, 30);
        assert!(matches!(d, Decision::Failed { why } if why.contains("30 minutes")));
    }

    #[test]
    fn gh_errors_fail_only_on_the_fifth_in_a_row_and_an_answer_resets() {
        let mut e = GhErrors::default();
        for _ in 0..4 {
            assert_eq!(e.error(), AfterError::Retry(Duration::from_secs(60)));
        }
        e.answered();
        for _ in 0..4 {
            assert!(matches!(e.error(), AfterError::Retry(_)));
        }
        assert_eq!(e.error(), AfterError::Failed);
    }

    #[cfg(unix)]
    #[test]
    fn a_failing_checks_log_is_cut_to_its_last_120_lines() {
        let dir = tempfile::tempdir().unwrap();
        let gh = dir.path().join("fake_gh.sh");
        let script = "#!/bin/sh\ncase \"$*\" in \"run view 42 --log-failed\") i=1; while [ $i -le 300 ]; do echo \"line $i\"; i=$((i+1)); done;; esac\n";
        crate::test_support::write_executable(&gh, script);
        let f = Failing {
            name: "t".into(),
            link: "https://github.com/o/r/actions/runs/42/job/9".into(),
        };
        let e = red_entry(&gh, dir.path(), &f, &|_| {});
        let lines: Vec<&str> = e.log.lines().collect();
        assert_eq!(lines.len(), 120);
        assert_eq!((lines[0], lines[119]), ("line 181", "line 300"));
        assert_eq!((e.name.as_str(), e.link.as_str()), ("t", f.link.as_str()));
        let none = red_entry(
            &gh,
            dir.path(),
            &Failing {
                name: "x".into(),
                link: "https://x/y".into(),
            },
            &|_| {},
        );
        assert_eq!(none.log, "");
    }

    #[cfg(unix)]
    #[test]
    fn read_checks_reads_failing_output_but_errors_on_a_gh_failure() {
        let dir = tempfile::tempdir().unwrap();
        let gh = dir.path().join("fake_gh.sh");
        crate::test_support::write_executable(
            &gh,
            "#!/bin/sh\ncase \"$*\" in *--json*) echo '[{\"name\":\"a\",\"bucket\":\"pending\",\"startedAt\":\"0001-01-01T00:00:00Z\"}]';; *) printf 'a\\tpending\\t0\\thttps://x/a\\t\\nb\\tfail\\t1s\\thttps://x/b\\t\\n'; exit 8;; esac\n",
        );
        let got = read_checks(&gh, dir.path(), "7", &|_| {}).unwrap();
        assert_eq!(
            (got.len(), got[0].started, got[1].state),
            (2, Some(false), CheckState::Fail)
        );
        let bad = dir.path().join("bad.sh");
        crate::test_support::write_executable(&bad, "#!/bin/sh\necho 'HTTP 502' >&2\nexit 1\n");
        assert!(read_checks(&bad, dir.path(), "7", &|_| {}).is_err());
        let none = dir.path().join("none.sh");
        crate::test_support::write_executable(
            &none,
            "#!/bin/sh\necho \"no checks reported on the 'x' branch\" >&2\nexit 1\n",
        );
        assert!(read_checks(&none, dir.path(), "7", &|_| {}).unwrap().is_empty());
    }
}
