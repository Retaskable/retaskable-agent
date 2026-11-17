//! A `lighthouse` task (`lighthouse()` in `jobTasks.ts`, `lighthouse.ts` and `lighthouseChore.ts`; jobs-on-server.md
//! section 6.2): Lighthouse on each page of the staging site with its mobile and desktop presets, as an orchestrator
//! command (no session, no model). Each report is kept on disk for the store ([`super::lighthouse_reports`]),
//! summarised ([`super::lighthouse_summary`]), and the day's raw result answered. The store works out what is new.
use std::collections::{BTreeMap, HashSet};
use std::future::Future;
use std::path::Path;

use serde::Serialize;

use super::lighthouse_reports::{ReportFlush, ReportStore};
use super::lighthouse_summary::{PageSummary, RawPage, Report, SummaryOptions, host_of, raw_pages, summarize};
use super::{ChromeFlag, JobSpec, Outcome, one_line, same_origin, wrong_url};
use crate::config::ProjectConfig;
use crate::lighthouse::run::{LighthouseOutcome, Preset, prune_reports, report_file, run_lighthouse};

/// Why no Chrome means no run (`NO_CHROME_NOTE`).
pub const NO_CHROME_NOTE: &str = "Lighthouse needs Chrome on this computer";
/// `MAX_FINDINGS` (shared/findingDrafts.ts): the most findings a result carries.
pub const MAX_FINDINGS: usize = 40;
/// `LIGHTHOUSE_RAW_NOTE_MAX` is 2000; the run's own notes are cut shorter than that.
const NOTE_MAX: usize = 300;

/// The project's `lighthouse` and `deploy.staging.url` settings, as the job reads them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LighthouseSettings {
    pub enabled: bool,
    /// `deploy.staging.url`, as saved.
    pub staging_url: Option<String>,
    pub pages: Vec<String>,
    pub min_score: u32,
}

impl LighthouseSettings {
    pub fn from_config(cfg: &ProjectConfig) -> Self {
        LighthouseSettings {
            enabled: cfg.lighthouse.enabled,
            staging_url: cfg.deploy.staging.as_ref().and_then(|s| s.url.clone()),
            pages: cfg.lighthouse.pages.clone(),
            min_score: cfg.lighthouse.min_score,
        }
    }
}

/// What the chore measures (`LighthouseTarget`).
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    /// The staging url, without a trailing slash.
    pub url: String,
    pub host: String,
    pub pages: Vec<String>,
    pub min_score: u32,
}

/// `lighthouseTarget`: what the chore measures, `Err(note)` when it's on but can't run (Setup says why), or `None`
/// when it's off.
pub fn target(s: &LighthouseSettings) -> Option<Result<Target, String>> {
    if !s.enabled {
        return None;
    }
    let url = s
        .staging_url
        .as_deref()
        .unwrap_or("")
        .trim()
        .trim_end_matches('/')
        .to_string();
    if url.is_empty() {
        return Some(Err(
            "Lighthouse needs a staging url (deploy.staging.url) to run on.".to_string()
        ));
    }
    if reqwest::Url::parse(&url).is_err() {
        return Some(Err(format!(
            "Lighthouse can't run on the staging url {url}: it isn't a url."
        )));
    }
    let pages: Vec<String> = s
        .pages
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .map(|p| {
            if p.starts_with('/') {
                p.to_string()
            } else {
                format!("/{p}")
            }
        })
        .collect();
    Some(Ok(Target {
        host: host_of(&url),
        url,
        pages: if pages.is_empty() { vec!["/".to_string()] } else { pages },
        min_score: s.min_score,
    }))
}

/// One Lighthouse run (`runLighthouse`): the real one is [`NpxLighthouse`].
pub trait LighthouseRunner {
    /// Runs Lighthouse on `url` with `preset`, writing its JSON report to `output`.
    fn run(&self, url: &str, preset: Preset, output: &Path) -> impl Future<Output = LighthouseOutcome> + Send;
}

/// `npx lighthouse@12`, run off the async threads.
pub struct NpxLighthouse;

impl LighthouseRunner for NpxLighthouse {
    async fn run(&self, url: &str, preset: Preset, output: &Path) -> LighthouseOutcome {
        let (url, output) = (url.to_string(), output.to_path_buf());
        tokio::task::spawn_blocking(move || run_lighthouse(&url, preset, &output))
            .await
            .unwrap_or_else(|e| LighthouseOutcome::Failed(format!("{e}")))
    }
}

/// What the task and this computer's settings give the run.
pub struct LighthouseInput<'a> {
    /// `day` and `args.url`.
    pub job: &'a JobSpec,
    pub settings: &'a LighthouseSettings,
    /// The project's own folder, where `lighthouse/<day>/` keeps the reports (`projectDir`).
    pub project_dir: &'a Path,
    /// An old agent's own record of the findings it wrote up, sent for the store to take once.
    pub known: BTreeMap<String, String>,
    pub chrome: &'a ChromeFlag,
}

/// A finding as the result carries it, cut to the store's limits.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultFinding {
    pub key: String,
    pub page: String,
    pub preset: Preset,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_value: Option<String>,
    pub items: Vec<String>,
}

/// The day's raw result (`LighthouseRaw`): `lighthouse_raw` in the store.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LighthouseResult {
    pub day: String,
    /// Which page in `pages` the config's first page is: `days`, `a11y` and `security` only ever track it.
    pub first_page: String,
    pub pages: Vec<RawPage>,
    /// The day's failing audits and categories under the minimum, for the write-up the store makes.
    pub findings: Vec<ResultFinding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

fn cut(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// The day's findings: a key once, none over 400 characters, at most [`MAX_FINDINGS`], each cut to what the store keeps.
pub fn result_findings(summaries: &[PageSummary]) -> Vec<ResultFinding> {
    let mut seen = HashSet::new();
    summaries
        .iter()
        .flat_map(|s| s.findings.iter())
        .filter(|f| f.key.chars().count() <= 400 && seen.insert(f.key.clone()))
        .take(MAX_FINDINGS)
        .map(|f| ResultFinding {
            key: f.key.clone(),
            page: cut(&f.page, 300),
            preset: f.preset,
            title: cut(&f.title, 200),
            display_value: f.display_value.as_deref().map(|d| cut(d, 100)),
            items: f.items.iter().take(3).map(|i| cut(i, 200)).collect(),
        })
        .collect()
}

/// The `lighthouse` job. `store` takes the reports from disk (none: they stay, as in tests).
pub async fn lighthouse_job<R: LighthouseRunner, S: ReportStore>(
    input: &LighthouseInput<'_>,
    runner: &R,
    store: Option<&S>,
    flush: &ReportFlush,
) -> Outcome<LighthouseResult> {
    let t = match target(input.settings) {
        None => return Outcome::refused("Lighthouse isn't on for this project."),
        Some(Err(note)) => return Outcome::refused(note),
        Some(Ok(t)) => t,
    };
    if let Some(url) = input.job.arg_str("url")
        && !same_origin(url, &t.url)
    {
        return wrong_url("Lighthouse won't run");
    }
    let day = input.job.day.as_str();
    let mut summaries = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    for page in &t.pages {
        for preset in Preset::ALL {
            let url = format!("{}{page}", t.url);
            let out = report_file(input.project_dir, day, page, preset);
            match runner.run(&url, preset, &out).await {
                LighthouseOutcome::Ok(path) => match std::fs::read_to_string(&path) {
                    Ok(text) if serde_json::from_str::<serde_json::Value>(&text).is_ok() => {
                        let opts = SummaryOptions {
                            page,
                            preset,
                            host: &t.host,
                            min_score: t.min_score,
                            staging: true,
                        };
                        summaries.push(summarize(&Report::parse(&text), &opts));
                    }
                    Ok(_) => errors.push(format!(
                        "Lighthouse's report on {url} ({}) couldn't be read: not JSON",
                        preset.name()
                    )),
                    Err(e) => errors.push(format!(
                        "Lighthouse's report on {url} ({}) couldn't be read: {e}",
                        preset.name()
                    )),
                },
                LighthouseOutcome::ChromeMissing => {
                    input.chrome.mark_missing();
                    return Outcome::refused(NO_CHROME_NOTE);
                }
                LighthouseOutcome::Failed(e) => errors.push(e),
            }
        }
    }
    prune_reports(input.project_dir, day);
    flush.flush(input.project_dir, store, true).await;
    if summaries.is_empty() {
        let first = errors.first().map_or("no run", String::as_str);
        return Outcome::failed(format!("Lighthouse failed on every page: {first}"));
    }
    let note = errors.first().map(|first| {
        one_line(
            &format!(
                "{} of {} runs failed: {first}",
                errors.len(),
                t.pages.len() * Preset::ALL.len()
            ),
            NOTE_MAX,
        )
    });
    Outcome::Done {
        result: LighthouseResult {
            day: day.to_string(),
            first_page: t.pages[0].clone(),
            pages: raw_pages(&summaries),
            findings: result_findings(&summaries),
            note,
        },
        known: input.known.clone(),
    }
}

#[cfg(test)]
mod tests;
