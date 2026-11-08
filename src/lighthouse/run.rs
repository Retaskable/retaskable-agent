//! Running Lighthouse for one page and preset, and keeping its reports until they are uploaded
//! (docs/specs/daily-lighthouse.md sections 2 and 3; Retaskable.com's `agent/src/lighthouse.ts`:
//! `runLighthouse`, `reportFile`, `pruneReports`). An orchestrator command like the checks, never a session.
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use chrono::NaiveDate;

use crate::proc;

/// Each run's limit.
pub const RUN_TIMEOUT: Duration = Duration::from_secs(3 * 60);
/// Day folders older than this many days are removed.
pub const KEEP_DAYS: i64 = 14;
const PINNED: &str = "lighthouse@12";

/// What one Lighthouse run came to.
#[derive(Debug, Clone, PartialEq)]
pub enum RunOutcome {
    /// The JSON report is at this path.
    Ok(PathBuf),
    /// Lighthouse found no Chrome on this computer.
    ChromeMissing,
    /// It failed or timed out, with the end of its output or the reason.
    Failed(String),
}

/// `lighthouse/<day>/<page>-<preset>.json` under the project's folder (`reportFile`).
pub fn report_file(project_dir: &Path, day: &str, page: &str, preset: &str) -> PathBuf {
    let slug: String = page
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let slug = slug.trim_matches('-');
    let slug = if slug.is_empty() { "home" } else { slug };
    lighthouse_dir(project_dir)
        .join(day)
        .join(format!("{slug}-{preset}.json"))
}

fn lighthouse_dir(project_dir: &Path) -> PathBuf {
    project_dir.join("lighthouse")
}

/// Removes day folders (named `yyyy-mm-dd`) more than [`KEEP_DAYS`] before `today`; anything else stays.
pub fn prune_reports(project_dir: &Path, today: NaiveDate) {
    let Ok(entries) = std::fs::read_dir(lighthouse_dir(project_dir)) else {
        return;
    };
    for entry in entries.flatten() {
        let old = entry
            .file_name()
            .to_str()
            .and_then(|n| NaiveDate::parse_from_str(n, "%Y-%m-%d").ok())
            .is_some_and(|d| (today - d).num_days() > KEEP_DAYS);
        if old && entry.path().is_dir() {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// The arguments after `npx`; `desktop` adds `--preset=desktop`.
pub fn lighthouse_args(url: &str, preset: &str, output: &Path) -> Vec<String> {
    let mut args = vec![
        "--yes".to_string(),
        PINNED.to_string(),
        url.to_string(),
        "--output=json".to_string(),
        format!("--output-path={}", output.display()),
        "--chrome-flags=--headless=new".to_string(),
        "--quiet".to_string(),
    ];
    if preset == "desktop" {
        args.push("--preset=desktop".to_string());
    }
    args
}

/// `npx` and its args as one line for the shell: double quotes on Windows, single quotes elsewhere.
pub fn command_line(args: &[String], windows: bool) -> String {
    let quoted = args.iter().map(|a| {
        if windows {
            format!("\"{}\"", a.replace('"', "\\\""))
        } else {
            format!("'{}'", a.replace('\'', "'\\''"))
        }
    });
    std::iter::once("npx".to_string())
        .chain(quoted)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Lighthouse's own words for a computer with no Chrome.
fn chrome_missing(output: &str) -> bool {
    let o = output.to_lowercase();
    o.contains("no chrome installations found") || o.contains("unable to find chrome") || o.contains("chrome_path")
}

/// Runs Lighthouse on `url` with `preset` (`mobile` or `desktop`), writing `output`. `CHROME_PATH` is
/// passed on in the environment when set.
pub fn run_lighthouse(url: &str, preset: &str, output: &Path) -> RunOutcome {
    run_lighthouse_with(url, preset, output, RUN_TIMEOUT, proc::run_command_with_timeout)
}

fn run_lighthouse_with(
    url: &str,
    preset: &str,
    output: &Path,
    timeout: Duration,
    runner: impl Fn(&str, &Path, Duration) -> Result<(i32, String, bool)>,
) -> RunOutcome {
    let Some(dir) = output.parent() else {
        return RunOutcome::Failed("no report folder".into());
    };
    if let Err(e) = std::fs::create_dir_all(dir) {
        return RunOutcome::Failed(format!("making {}: {e}", dir.display()));
    }
    let command = command_line(&lighthouse_args(url, preset, output), cfg!(windows));
    match runner(&command, dir, timeout) {
        Err(e) => RunOutcome::Failed(format!("{e:#}")),
        Ok((_, _, true)) => RunOutcome::Failed(format!("timed out after {} seconds", timeout.as_secs())),
        Ok((code, out, false)) => {
            if chrome_missing(&out) {
                RunOutcome::ChromeMissing
            } else if code == 0 && output.is_file() {
                RunOutcome::Ok(output.to_path_buf())
            } else {
                RunOutcome::Failed(format!("exit {code}: {}", proc::tail_lines(&out, 10)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn args_for_mobile_and_desktop() {
        let out = Path::new("r.json");
        let m = lighthouse_args("https://s.example/", "mobile", out);
        assert_eq!(
            m,
            [
                "--yes",
                "lighthouse@12",
                "https://s.example/",
                "--output=json",
                "--output-path=r.json",
                "--chrome-flags=--headless=new",
                "--quiet"
            ]
        );
        let d = lighthouse_args("https://s.example/", "desktop", out);
        assert_eq!(d.last().unwrap(), "--preset=desktop");
        assert_eq!(d.len(), m.len() + 1);
    }

    #[test]
    fn command_line_quotes_per_platform() {
        let a = vec!["a b".to_string(), "it's".to_string()];
        assert_eq!(command_line(&a, true), "npx \"a b\" \"it's\"");
        assert_eq!(command_line(&a, false), "npx 'a b' 'it'\\''s'");
    }

    #[test]
    fn a_report_is_written_gives_its_path() {
        let d = tmp();
        let out = report_file(d.path(), "2026-11-08", "/", "mobile");
        let o = out.clone();
        let r = run_lighthouse_with("u", "mobile", &out, RUN_TIMEOUT, move |_, _, _| {
            std::fs::write(&o, "{}").unwrap();
            Ok((0, String::new(), false))
        });
        assert_eq!(r, RunOutcome::Ok(out));
    }

    #[test]
    fn no_chrome_is_its_own_outcome() {
        let d = tmp();
        let out = report_file(d.path(), "2026-11-08", "/", "mobile");
        let r = run_lighthouse_with("u", "mobile", &out, RUN_TIMEOUT, |_, _, _| {
            Ok((1, "Runtime error: No Chrome installations found.".into(), false))
        });
        assert_eq!(r, RunOutcome::ChromeMissing);
    }

    #[test]
    fn a_failure_without_a_report_is_failed() {
        let d = tmp();
        let out = report_file(d.path(), "2026-11-08", "/", "mobile");
        let r = run_lighthouse_with("u", "mobile", &out, RUN_TIMEOUT, |_, _, _| {
            Ok((1, "boom".into(), false))
        });
        assert!(matches!(r, RunOutcome::Failed(m) if m.contains("boom")));
    }

    #[cfg(not(windows))]
    #[test]
    fn a_run_past_the_timeout_is_killed_and_failed() {
        let d = tmp();
        let out = report_file(d.path(), "2026-11-08", "/", "mobile");
        let started = std::time::Instant::now();
        let r = run_lighthouse_with("u", "mobile", &out, Duration::from_secs(1), |_, cwd, t| {
            proc::run_command_with_timeout("sleep 30", cwd, t)
        });
        assert!(matches!(r, RunOutcome::Failed(m) if m.contains("timed out")));
        assert!(started.elapsed() < Duration::from_secs(20));
    }

    #[test]
    fn report_paths_are_per_day_page_and_preset() {
        let p = Path::new("proj");
        let base = p.join("lighthouse").join("2026-11-08");
        assert_eq!(
            report_file(p, "2026-11-08", "/", "mobile"),
            base.join("home-mobile.json")
        );
        assert_eq!(
            report_file(p, "2026-11-08", "/docs/intro", "desktop"),
            base.join("docs-intro-desktop.json")
        );
    }

    #[test]
    fn prune_removes_only_day_folders_older_than_14_days() {
        let d = tmp();
        let root = d.path().join("lighthouse");
        for n in ["2026-10-01", "2026-10-25", "2026-10-26", "2026-11-08", "notes"] {
            std::fs::create_dir_all(root.join(n)).unwrap();
        }
        prune_reports(d.path(), NaiveDate::from_ymd_opt(2026, 11, 8).unwrap());
        let mut left: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(left, ["2026-10-25", "2026-10-26", "2026-11-08", "notes"]);
    }
}
