//! Running Lighthouse for one page and preset, and keeping its reports until they are uploaded
//! (docs/specs/daily-lighthouse.md sections 2 "Lighthouse and Chrome" and 3 "Plain code runs it";
//! `runLighthouse`, `reportFile`, `pruneReports` in `agent/src/lighthouse.ts`). An orchestrator
//! command, outside any session, like the checks.
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::NaiveDate;

use crate::proc;

/// Each run's time limit.
pub const RUN_TIMEOUT: Duration = Duration::from_secs(3 * 60);
/// Reports older than this many days are pruned.
pub const KEEP_DAYS: i64 = 14;
/// The pinned Lighthouse major.
const LIGHTHOUSE: &str = "lighthouse@12";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    Mobile,
    Desktop,
}

impl Preset {
    pub fn name(self) -> &'static str {
        match self {
            Preset::Mobile => "mobile",
            Preset::Desktop => "desktop",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum LighthouseOutcome {
    /// The JSON report was written here.
    Ok(PathBuf),
    /// Lighthouse found no Chrome on this computer.
    ChromeMissing,
    /// Timed out, exited with an error, or wrote no report; the text says why.
    Failed(String),
}

/// The arguments after `npx`.
pub fn lighthouse_args(url: &str, preset: Preset, output: &Path) -> Vec<String> {
    let mut args = vec![
        "--yes".to_string(),
        LIGHTHOUSE.to_string(),
        url.to_string(),
        "--output=json".to_string(),
        format!("--output-path={}", output.display()),
        "--chrome-flags=--headless=new".to_string(),
        "--quiet".to_string(),
    ];
    if preset == Preset::Desktop {
        args.push("--preset=desktop".to_string());
    }
    args
}

/// One shell command line: every word quoted for `cmd` (Windows) or `sh`.
pub fn command_line(program: &str, args: &[String], windows: bool) -> String {
    std::iter::once(program)
        .chain(args.iter().map(String::as_str))
        .map(|w| quote(w, windows))
        .collect::<Vec<_>>()
        .join(" ")
}

fn quote(word: &str, windows: bool) -> String {
    if windows {
        format!("\"{}\"", word.replace('"', "\\\""))
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

/// Lighthouse's own words for no Chrome (chrome-launcher's).
fn chrome_missing(output: &str) -> bool {
    let lower = output.to_lowercase();
    lower.contains("no chrome installations found") || lower.contains("unable to find chrome")
}

/// Runs Lighthouse on `url` with `preset`, writing its JSON report to `output`. `CHROME_PATH` is
/// passed through from the environment when set.
pub fn run_lighthouse(url: &str, preset: Preset, output: &Path) -> LighthouseOutcome {
    run_with("npx", url, preset, output, RUN_TIMEOUT)
}

fn run_with(program: &str, url: &str, preset: Preset, output: &Path, timeout: Duration) -> LighthouseOutcome {
    let dir = output.parent().unwrap_or_else(|| Path::new("."));
    if let Err(e) = std::fs::create_dir_all(dir) {
        return LighthouseOutcome::Failed(format!("creating {}: {e}", dir.display()));
    }
    let command = command_line(program, &lighthouse_args(url, preset, output), cfg!(windows));
    match proc::run_command_with_timeout(&command, dir, timeout) {
        Err(e) => LighthouseOutcome::Failed(format!("{e:#}")),
        Ok((_, _, true)) => LighthouseOutcome::Failed(format!("timed out after {} seconds", timeout.as_secs())),
        Ok((code, text, false)) => {
            if chrome_missing(&text) {
                LighthouseOutcome::ChromeMissing
            } else if code == 0 && output.is_file() {
                LighthouseOutcome::Ok(output.to_path_buf())
            } else {
                LighthouseOutcome::Failed(format!("exit {code}: {}", proc::tail_lines(&text, 5)))
            }
        }
    }
}

/// Where one page and preset's report is kept for `day` (`yyyy-mm-dd`):
/// `<project_dir>/lighthouse/<day>/<page>-<preset>.json`.
pub fn report_file(project_dir: &Path, day: &str, page: &str, preset: Preset) -> PathBuf {
    let slug: String = page
        .trim_matches('/')
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let slug = if slug.is_empty() { "home" } else { slug.as_str() };
    project_dir
        .join("lighthouse")
        .join(day)
        .join(format!("{slug}-{}.json", preset.name()))
}

/// Removes the day folders under `<project_dir>/lighthouse` that are more than [`KEEP_DAYS`] days
/// before `today` (`yyyy-mm-dd`). Folders that aren't dates are left alone.
pub fn prune_reports(project_dir: &Path, today: &str) {
    let Ok(today) = NaiveDate::parse_from_str(today, "%Y-%m-%d") else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(project_dir.join("lighthouse")) else {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_for_mobile_and_desktop() {
        let out = Path::new("r.json");
        let mobile = lighthouse_args("https://s.example/", Preset::Mobile, out);
        assert_eq!(
            mobile,
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
        let desktop = lighthouse_args("https://s.example/", Preset::Desktop, out);
        assert_eq!(desktop.last().unwrap(), "--preset=desktop");
        assert_eq!(desktop.len(), mobile.len() + 1);
    }

    #[test]
    fn command_line_quotes_per_shell() {
        let args = vec!["a b".to_string(), "--x=\"y\"".to_string()];
        assert_eq!(command_line("npx", &args, true), r#""npx" "a b" "--x=\"y\"""#);
        assert_eq!(command_line("npx", &["it's".to_string()], false), r"'npx' 'it'\''s'");
    }

    #[cfg(unix)]
    fn fake(dir: &Path, body: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("fake-npx");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[cfg(unix)]
    #[test]
    fn a_report_written_is_ok() {
        let tmp = tempfile::tempdir().unwrap();
        let program = fake(
            tmp.path(),
            r#"for a in "$@"; do case "$a" in --output-path=*) echo '{}' > "${a#--output-path=}";; esac; done"#,
        );
        let out = report_file(tmp.path(), "2026-11-08", "/", Preset::Mobile);
        let got = run_with(
            &program,
            "https://s.example/",
            Preset::Mobile,
            &out,
            Duration::from_secs(20),
        );
        assert_eq!(got, LighthouseOutcome::Ok(out.clone()));
        assert!(out.is_file());
    }

    #[cfg(unix)]
    #[test]
    fn no_chrome_is_its_own_outcome() {
        let tmp = tempfile::tempdir().unwrap();
        let program = fake(
            tmp.path(),
            "echo 'Runtime error encountered: No Chrome installations found.' >&2; exit 1",
        );
        let out = report_file(tmp.path(), "2026-11-08", "/", Preset::Desktop);
        let got = run_with(
            &program,
            "https://s.example/",
            Preset::Desktop,
            &out,
            Duration::from_secs(20),
        );
        assert_eq!(got, LighthouseOutcome::ChromeMissing);
    }

    #[cfg(unix)]
    #[test]
    fn a_failure_or_a_timeout_is_failed() {
        let tmp = tempfile::tempdir().unwrap();
        let out = report_file(tmp.path(), "2026-11-08", "/", Preset::Mobile);
        let program = fake(tmp.path(), "echo boom; exit 3");
        let LighthouseOutcome::Failed(why) = run_with(&program, "u", Preset::Mobile, &out, Duration::from_secs(20))
        else {
            panic!("expected Failed");
        };
        assert!(why.contains("exit 3") && why.contains("boom"), "{why}");
        let program = fake(tmp.path(), "sleep 30");
        let started = std::time::Instant::now();
        let got = run_with(&program, "u", Preset::Mobile, &out, Duration::from_secs(1));
        assert!(
            matches!(&got, LighthouseOutcome::Failed(w) if w.contains("timed out")),
            "{got:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(15));
    }

    #[test]
    fn report_paths_are_per_day_page_and_preset() {
        let base = Path::new("p");
        let f = |page, preset| report_file(base, "2026-11-08", page, preset);
        let expect = |name: &str| ["p", "lighthouse", "2026-11-08", name].iter().collect::<PathBuf>();
        assert_eq!(f("/", Preset::Mobile), expect("home-mobile.json"));
        assert_eq!(f("/", Preset::Desktop), expect("home-desktop.json"));
        assert_eq!(f("/docs/intro", Preset::Mobile), expect("docs_intro-mobile.json"));
    }

    #[test]
    fn pruning_removes_only_old_day_folders() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("lighthouse");
        for d in ["2026-10-01", "2026-10-24", "2026-10-25", "2026-11-08", "notes"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        prune_reports(tmp.path(), "2026-11-08");
        let mut left: Vec<String> = std::fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["2026-10-25", "2026-11-08", "notes"]);
    }
}
