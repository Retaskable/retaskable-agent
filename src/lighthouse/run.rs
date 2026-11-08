//! Running Lighthouse for one page and preset, and keeping its reports until they are uploaded
//! (docs/specs/daily-lighthouse.md sections 2 "Lighthouse and Chrome" and 3 "Plain code runs it";
//! Retaskable.com's `agent/src/lighthouse.ts`: `runLighthouse`, `reportFile`, `pruneReports`).
//! The command is the orchestrator's own, outside any session, run like a check.
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{Days, NaiveDate};

use crate::proc;

/// How long one run is given.
pub const RUN_TIMEOUT: Duration = Duration::from_secs(3 * 60);
/// Pinned major of Lighthouse.
const LIGHTHOUSE: &str = "lighthouse@12";
/// Reports of days older than this many days are removed.
const KEEP_DAYS: u64 = 14;
/// How much of a failed run's output is kept in its message.
const TAIL_LINES: usize = 20;

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

/// How one run ended.
#[derive(Debug, Clone, PartialEq)]
pub enum LighthouseRun {
    /// The JSON report was written here.
    Ok(PathBuf),
    /// Lighthouse found no Chrome on this computer.
    ChromeMissing,
    Failed(String),
}

/// The arguments after `npx` for one run.
pub fn lighthouse_args(url: &str, preset: Preset, out: &Path) -> Vec<String> {
    let mut args = vec![
        "--yes".to_string(),
        LIGHTHOUSE.to_string(),
        url.to_string(),
        "--output=json".to_string(),
        format!("--output-path={}", out.display()),
        "--chrome-flags=--headless=new".to_string(),
        "--quiet".to_string(),
    ];
    if preset == Preset::Desktop {
        args.push("--preset=desktop".to_string());
    }
    args
}

/// `program` and `args` as one command line for the shell: double quotes on Windows (`cmd`), single quotes elsewhere.
fn command_line(program: &str, args: &[String], windows: bool) -> String {
    let quote = |a: &str| {
        if windows {
            format!("\"{}\"", a.replace('"', "\"\""))
        } else {
            format!("'{}'", a.replace('\'', "'\\''"))
        }
    };
    let mut line = program.to_string();
    for a in args {
        line.push(' ');
        line.push_str(&quote(a));
    }
    line
}

/// Lighthouse's own words when it can't find Chrome.
fn chrome_missing(output: &str) -> bool {
    let o = output.to_lowercase();
    o.contains("no chrome installations found") || o.contains("unable to find chrome")
}

/// Runs Lighthouse on `url` and writes its JSON to `out`. `CHROME_PATH`, when set, is passed through as the
/// environment.
pub fn run_lighthouse(url: &str, preset: Preset, out: &Path) -> LighthouseRun {
    run_with("npx", url, preset, out, RUN_TIMEOUT)
}

fn run_with(program: &str, url: &str, preset: Preset, out: &Path, timeout: Duration) -> LighthouseRun {
    let dir = out.parent().unwrap_or(Path::new("."));
    if let Err(e) = std::fs::create_dir_all(dir) {
        return LighthouseRun::Failed(format!("creating {}: {e}", dir.display()));
    }
    let line = command_line(program, &lighthouse_args(url, preset, out), cfg!(windows));
    match proc::run_command_with_timeout(&line, dir, timeout) {
        Err(e) => LighthouseRun::Failed(format!("{e:#}")),
        Ok((_, _, true)) => LighthouseRun::Failed(format!("timed out after {}s", timeout.as_secs())),
        Ok((code, output, false)) => {
            if chrome_missing(&output) {
                LighthouseRun::ChromeMissing
            } else if code == 0 && out.is_file() {
                LighthouseRun::Ok(out.to_path_buf())
            } else {
                LighthouseRun::Failed(format!("exit {code}: {}", proc::tail_lines(&output, TAIL_LINES)))
            }
        }
    }
}

/// Where one page and preset's report is kept for `day` (`yyyy-mm-dd`).
pub fn report_file(project_dir: &Path, day: &str, page: &str, preset: Preset) -> PathBuf {
    let slug: String = page
        .trim_matches('/')
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' })
        .collect();
    let slug = if slug.is_empty() { "home" } else { &slug };
    project_dir
        .join("lighthouse")
        .join(day)
        .join(format!("{slug}-{}.json", preset.name()))
}

/// Removes day folders older than 14 days before `today`. Anything that isn't a dated folder stays.
pub fn prune_reports(project_dir: &Path, today: NaiveDate) {
    let Some(cutoff) = today.checked_sub_days(Days::new(KEEP_DAYS)) else {
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
            .is_some_and(|d| d < cutoff);
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
    fn quoting_per_platform() {
        let args = vec!["a b".to_string(), "it's".to_string()];
        assert_eq!(command_line("npx", &args, true), "npx \"a b\" \"it's\"");
        assert_eq!(command_line("npx", &args, false), "npx 'a b' 'it'\\''s'");
    }

    #[test]
    fn report_paths_per_day_page_and_preset() {
        let p = Path::new("proj");
        let root = report_file(p, "2026-11-08", "/", Preset::Mobile);
        assert_eq!(root, p.join("lighthouse").join("2026-11-08").join("home-mobile.json"));
        let deep = report_file(p, "2026-11-08", "/a/b.html", Preset::Desktop);
        assert_eq!(
            deep,
            p.join("lighthouse").join("2026-11-08").join("a_b_html-desktop.json")
        );
    }

    #[test]
    fn prune_removes_only_old_day_folders() {
        let t = tempfile::tempdir().unwrap();
        let base = t.path().join("lighthouse");
        for d in ["2026-10-24", "2026-10-25", "2026-11-08", "notes"] {
            std::fs::create_dir_all(base.join(d)).unwrap();
        }
        prune_reports(t.path(), NaiveDate::from_ymd_opt(2026, 11, 8).unwrap());
        assert!(!base.join("2026-10-24").exists());
        assert!(base.join("2026-10-25").exists());
        assert!(base.join("2026-11-08").exists());
        assert!(base.join("notes").exists());
    }

    #[cfg(unix)]
    mod fake {
        use super::*;

        /// A script standing in for `npx`: `body` runs with `$out` set to the `--output-path` value.
        fn fake(dir: &Path, body: &str) -> String {
            let script = dir.join("fake.sh");
            let text = format!(
                "#!/bin/sh\nfor a in \"$@\"; do case \"$a\" in --output-path=*) out=\"${{a#--output-path=}}\";; esac; done\n{body}\n"
            );
            std::fs::write(&script, text).unwrap();
            format!("sh {}", script.display())
        }

        #[test]
        fn a_written_report_is_ok() {
            let t = tempfile::tempdir().unwrap();
            let program = fake(t.path(), "echo '{}' > \"$out\"");
            let out = t.path().join("r").join("m.json");
            let run = run_with(&program, "https://s/", Preset::Mobile, &out, Duration::from_secs(20));
            assert_eq!(run, LighthouseRun::Ok(out));
        }

        #[test]
        fn no_chrome_is_its_own_outcome() {
            let t = tempfile::tempdir().unwrap();
            let program = fake(t.path(), "echo 'No Chrome installations found.' >&2; exit 1");
            let out = t.path().join("m.json");
            let run = run_with(&program, "https://s/", Preset::Mobile, &out, Duration::from_secs(20));
            assert_eq!(run, LighthouseRun::ChromeMissing);
        }

        #[test]
        fn other_failures_and_timeouts_are_failed() {
            let t = tempfile::tempdir().unwrap();
            let out = t.path().join("m.json");
            let bad = fake(t.path(), "echo boom; exit 3");
            match run_with(&bad, "https://s/", Preset::Mobile, &out, Duration::from_secs(20)) {
                LighthouseRun::Failed(m) => assert!(m.contains("exit 3") && m.contains("boom"), "{m}"),
                other => panic!("{other:?}"),
            }
            let slow = fake(t.path(), "sleep 30");
            let started = std::time::Instant::now();
            match run_with(&slow, "https://s/", Preset::Mobile, &out, Duration::from_secs(1)) {
                LighthouseRun::Failed(m) => assert!(m.contains("timed out"), "{m}"),
                other => panic!("{other:?}"),
            }
            assert!(started.elapsed() < Duration::from_secs(20));
        }
    }
}
