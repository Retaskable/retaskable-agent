//! Running Lighthouse for one page and preset, and keeping its reports until they are uploaded
//! (docs/specs/daily-lighthouse.md sections 2 "Lighthouse and Chrome" and 3 "Plain code runs it";
//! Retaskable.com's `agent/src/lighthouse.ts`: `runLighthouse`, `reportFile`, `pruneReports`).
//! The orchestrator's own command, outside any session, like the checks.
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{Duration as Days, NaiveDate};

use crate::proc;

/// Each run's time limit.
const TIMEOUT: Duration = Duration::from_secs(180);
/// Reports are kept this many days.
const KEEP_DAYS: i64 = 14;
/// The words Lighthouse uses when it finds no Chrome.
const NO_CHROME: [&str; 2] = ["no chrome installations found", "unable to connect to chrome"];

/// How a run ended.
#[derive(Debug, Clone, PartialEq)]
pub enum LighthouseRun {
    /// The JSON report's path.
    Ok(PathBuf),
    /// Lighthouse found no Chrome on this computer.
    ChromeMissing,
    /// Anything else, with the reason.
    Failed(String),
}

/// `desktop` or `mobile`: the preset decides whether `--preset=desktop` is passed.
pub fn lighthouse_args(url: &str, preset: &str, output: &Path) -> Vec<String> {
    let mut args: Vec<String> = ["--yes", "lighthouse@12", url].iter().map(|s| s.to_string()).collect();
    args.push("--output=json".into());
    args.push(format!("--output-path={}", output.display()));
    args.push("--chrome-flags=--headless=new".into());
    args.push("--quiet".into());
    if preset == "desktop" {
        args.push("--preset=desktop".into());
    }
    args
}

/// `arg` quoted for the shell the helpers use: `cmd` on Windows, `sh` elsewhere.
fn quote(arg: &str, windows: bool) -> String {
    let plain = !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=@\\".contains(c));
    if plain {
        arg.to_string()
    } else if windows {
        format!("\"{}\"", arg.replace('"', "\"\""))
    } else {
        format!("'{}'", arg.replace('\'', "'\\''"))
    }
}

fn npx_command(url: &str, preset: &str, output: &Path, windows: bool) -> String {
    let args: Vec<String> = lighthouse_args(url, preset, output)
        .iter()
        .map(|a| quote(a, windows))
        .collect();
    format!("npx {}", args.join(" "))
}

/// Runs Lighthouse on `url` with `preset` (`mobile` or `desktop`), writing its JSON to `output`.
/// `CHROME_PATH` passes through from the environment.
pub fn run_lighthouse(url: &str, preset: &str, output: &Path) -> LighthouseRun {
    run_command(&npx_command(url, preset, output, cfg!(windows)), output, TIMEOUT)
}

fn run_command(command: &str, output: &Path, timeout: Duration) -> LighthouseRun {
    let dir = output.parent().unwrap_or(Path::new("."));
    if let Err(e) = std::fs::create_dir_all(dir) {
        return LighthouseRun::Failed(format!("making {}: {e}", dir.display()));
    }
    let _ = std::fs::remove_file(output);
    let (code, text, timed_out) = match proc::run_command_with_timeout(command, dir, timeout) {
        Ok(r) => r,
        Err(e) => return LighthouseRun::Failed(format!("{e:#}")),
    };
    if timed_out {
        return LighthouseRun::Failed(format!(
            "Lighthouse ran past {} seconds and was stopped",
            timeout.as_secs()
        ));
    }
    let lower = text.to_lowercase();
    if NO_CHROME.iter().any(|w| lower.contains(w)) {
        return LighthouseRun::ChromeMissing;
    }
    if code != 0 || !output.is_file() {
        return LighthouseRun::Failed(format!("Lighthouse exited {code}: {}", proc::tail_lines(&text, 5)));
    }
    LighthouseRun::Ok(output.to_path_buf())
}

/// Where one page and preset's report is kept for `day` (`yyyy-mm-dd`):
/// `<project dir>/lighthouse/<day>/<page>-<preset>.json`.
pub fn report_file(project_dir: &Path, day: &str, page: &str, preset: &str) -> PathBuf {
    let page: String = page
        .trim_matches('/')
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let page = if page.is_empty() { "index".to_string() } else { page };
    project_dir
        .join("lighthouse")
        .join(day)
        .join(format!("{page}-{preset}.json"))
}

/// Removes day folders older than 14 days before `today`; other entries stay.
pub fn prune_reports(project_dir: &Path, today: NaiveDate) {
    let Ok(entries) = std::fs::read_dir(project_dir.join("lighthouse")) else {
        return;
    };
    let cutoff = today - Days::days(KEEP_DAYS);
    for entry in entries.flatten() {
        let day = entry.file_name().to_string_lossy().parse::<NaiveDate>();
        if entry.path().is_dir() && day.is_ok_and(|d| d < cutoff) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out() -> PathBuf {
        PathBuf::from("/tmp/r.json")
    }

    #[test]
    fn mobile_and_desktop_arguments() {
        let mobile = lighthouse_args("https://s.example/", "mobile", &out());
        assert_eq!(
            mobile,
            [
                "--yes",
                "lighthouse@12",
                "https://s.example/",
                "--output=json",
                "--output-path=/tmp/r.json",
                "--chrome-flags=--headless=new",
                "--quiet"
            ]
        );
        let desktop = lighthouse_args("https://s.example/", "desktop", &out());
        assert_eq!(desktop.last().unwrap(), "--preset=desktop");
        assert_eq!(desktop.len(), mobile.len() + 1);
    }

    #[test]
    fn quoting_per_shell() {
        assert_eq!(quote("a b", true), "\"a b\"");
        assert_eq!(quote("a\"b", true), "\"a\"\"b\"");
        assert_eq!(quote("a b", false), "'a b'");
        assert_eq!(quote("it's", false), "'it'\\''s'");
        assert_eq!(quote("--quiet", true), "--quiet");
        let cmd = npx_command(
            "https://s.example/?a=1&b=2",
            "desktop",
            Path::new("C:\\my dir\\r.json"),
            true,
        );
        assert!(cmd.starts_with("npx --yes lighthouse@12 \"https://s.example/?a=1&b=2\""));
        assert!(cmd.contains("\"--output-path=C:\\my dir\\r.json\""));
    }

    #[cfg(unix)]
    mod fake {
        use super::*;

        fn run(script: &str, timeout: Duration) -> (LighthouseRun, PathBuf) {
            let dir = tempfile::tempdir().unwrap().keep();
            let file = report_file(&dir, "2026-11-08", "/", "mobile");
            (
                run_command(&script.replace("$OUT", &file.display().to_string()), &file, timeout),
                file,
            )
        }

        #[test]
        fn a_fake_that_writes_the_file_is_ok() {
            let (r, file) = run("echo '{}' > $OUT", Duration::from_secs(10));
            assert_eq!(r, LighthouseRun::Ok(file));
        }

        #[test]
        fn lighthouses_no_chrome_words_are_their_own_outcome() {
            let (r, _) = run("echo 'No Chrome installations found.'; exit 1", Duration::from_secs(10));
            assert_eq!(r, LighthouseRun::ChromeMissing);
        }

        #[test]
        fn a_failure_without_a_file_is_failed() {
            let (r, _) = run("echo boom; exit 2", Duration::from_secs(10));
            assert!(matches!(r, LighthouseRun::Failed(m) if m.contains("boom")));
        }

        #[test]
        fn a_run_past_the_timeout_is_killed_and_failed() {
            let start = std::time::Instant::now();
            let (r, _) = run("sleep 30", Duration::from_secs(1));
            assert!(matches!(r, LighthouseRun::Failed(m) if m.contains("stopped")));
            assert!(start.elapsed() < Duration::from_secs(15));
        }
    }

    #[test]
    fn report_paths_are_per_day_page_and_preset() {
        let p = Path::new("proj");
        let expected = |parts: &[&str]| parts.iter().collect::<PathBuf>();
        assert_eq!(
            report_file(p, "2026-11-08", "/", "mobile"),
            expected(&["proj", "lighthouse", "2026-11-08", "index-mobile.json"])
        );
        assert_eq!(
            report_file(p, "2026-11-08", "/docs/intro", "desktop"),
            expected(&["proj", "lighthouse", "2026-11-08", "docs-intro-desktop.json"])
        );
    }

    #[test]
    fn pruning_removes_only_day_folders_older_than_14_days() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("lighthouse");
        for d in ["2026-10-24", "2026-10-25", "2026-11-08", "notes"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::fs::write(root.join("2026-01-01"), "a file").unwrap();
        prune_reports(dir.path(), NaiveDate::from_ymd_opt(2026, 11, 8).unwrap());
        assert!(!root.join("2026-10-24").exists());
        for kept in ["2026-10-25", "2026-11-08", "notes", "2026-01-01"] {
            assert!(root.join(kept).exists(), "{kept}");
        }
    }
}
