//! An `a11y` task (`a11y()` in `jobTasks.ts`, `a11yChore.ts` and `a11yScan.ts`; compliance-scan.md section 3.3): the
//! accessibility sweep, axe-core against the staging site's routes at phone and desktop size, light and dark, plus
//! the checks axe can't make (keyboard walk, focus, reflow, text spacing, page titles). The sweep runs through a
//! [`Sweeper`] ([`super::a11y_sweep`]); this decides the target, refuses in words, and answers the raw result.
use std::collections::{BTreeMap, HashSet};

use serde::Serialize;
use serde_json::{Value, json};

use super::a11y_sweep::{SweepRun, Sweeper};
use super::scan::{A11Y_WORDS, ScanSettings, is_retaskable};
use super::{ChromeFlag, JobSpec, Outcome, one_line, same_origin, wrong_url};

/// Why no Chrome means no sweep (`NO_CHROME_NOTE`).
pub const NO_CHROME_NOTE: &str = "The accessibility sweep needs Chrome on this computer";
/// `MAX_A11Y_FAILURES` (shared/findingDrafts.ts).
pub const MAX_FAILURES: usize = 120;
/// `SCAN_A11Y_RAW_NOTE_MAX`.
pub const NOTE_MAX: usize = 2000;
/// A sweep over this is noted: it needs trimming.
const BUDGET_MS: u64 = 2 * 60_000;

/// The sweep's settings: the `scan` section, plus the Sample's screens for retaskable.com's own site.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct A11ySettings {
    pub scan: ScanSettings,
    /// `SAMPLE_SCREENS` (the app's `ROUTE_TABS` without the ones the Sample can't open): added as
    /// `/<screen>?sample` routes on retaskable.com's own site only. The agent has no list of the app's screens.
    pub sample_screens: Vec<String>,
}

/// What the sweep loads (`A11yTarget`).
#[derive(Debug, Clone, PartialEq)]
pub struct A11yTarget {
    pub url: String,
    pub pages: Vec<String>,
}

/// `a11yTarget`: what the sweep loads, `Err(note)` when it's on but can't run, or `None` when it's off. Never production.
pub fn a11y_target(s: &A11ySettings) -> Option<Result<A11yTarget, String>> {
    if !s.scan.a11y {
        return None;
    }
    let (origin, url) = match s.scan.staging_origin(&A11Y_WORDS) {
        Ok(v) => v,
        Err(note) => return Some(Err(note)),
    };
    let mut pages = s.scan.listed_pages();
    if pages.is_empty() {
        pages.push("/".to_string());
    }
    if is_retaskable(url.host_str().unwrap_or("")) {
        pages.extend(s.sample_screens.iter().map(|screen| format!("/{screen}?sample")));
    }
    Some(Ok(A11yTarget { url: origin, pages }))
}

/// `SAMPLE_PREFS`: stored the way the Sample's own prefs are, for retaskable.com's own screens, every screen on.
pub fn sample_prefs() -> Value {
    json!({
        "source": "sample",
        "navRevision": 6,
        "paletteRevision": 1,
        "sourceRevision": 1,
        "flags": {
            "board": true, "whiteboard": true, "chores": true, "filters": true, "chat": true, "agents": true,
            "releases": true, "galleries": true, "numbers": true, "requirements": true, "expert": true,
            "profiles": true, "settings": true
        }
    })
}

/// What the task and this computer's settings give the run.
pub struct A11yInput<'a> {
    /// `day` and `args.url`.
    pub job: &'a JobSpec,
    pub settings: &'a A11ySettings,
    /// An old agent's own record of the findings it wrote up.
    pub known: BTreeMap<String, String>,
    pub chrome: &'a ChromeFlag,
}

/// Each axe rule or check that ran: `pass[i]` is whether route i passed.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IdPass {
    pub id: String,
    pub pass: Vec<bool>,
}

/// Every failing occurrence, for the write-up the store makes: a route, size, theme and rule.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultFailure {
    pub key: String,
    pub route: String,
    pub size: String,
    pub theme: String,
    pub rule_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elements: Option<Vec<String>>,
}

/// The day's raw result (`ScanA11yRaw`): `scan_a11y_raw` in the store.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct A11yResult {
    pub day: String,
    pub routes: usize,
    pub rules: Vec<IdPass>,
    pub checks: Vec<IdPass>,
    pub ms: u64,
    pub failures: Vec<ResultFailure>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// The `a11y` job.
pub async fn a11y_job<S: Sweeper>(input: &A11yInput<'_>, sweeper: &S) -> Outcome<A11yResult> {
    let t = match a11y_target(input.settings) {
        None => return Outcome::refused("The accessibility sweep isn't on for this project."),
        Some(Err(note)) => return Outcome::refused(note),
        Some(Ok(t)) => t,
    };
    if let Some(url) = input.job.arg_str("url")
        && !same_origin(url, &t.url)
    {
        return wrong_url("The sweep won't run");
    }
    let prefs = t.pages.iter().any(|p| p.contains("?sample")).then(sample_prefs);
    let (result, ms) = match sweeper.sweep(&t.url, &t.pages, prefs.as_ref()).await {
        SweepRun::Ok { result, ms } => (result, ms),
        SweepRun::Failed { no_chrome: true, .. } => {
            input.chrome.mark_missing();
            return Outcome::refused(NO_CHROME_NOTE);
        }
        SweepRun::Failed { error, .. } => {
            return Outcome::failed(one_line(&format!("The accessibility sweep failed: {error}"), 300));
        }
    };
    let note = (ms > BUDGET_MS).then(|| {
        one_line(
            &format!(
                "The sweep took {}s, over its 2-minute budget: it needs trimming",
                (ms as f64 / 1000.0).round() as u64
            ),
            NOTE_MAX,
        )
    });
    let mut seen = HashSet::new();
    let failures = result
        .failures
        .iter()
        .filter(|f| f.key.chars().count() <= 300 && f.route.chars().count() <= 300 && seen.insert(f.key.clone()))
        .take(MAX_FAILURES)
        .map(|f| ResultFailure {
            key: f.key.clone(),
            route: f.route.clone(),
            size: f.size.name().to_string(),
            theme: f.theme.name().to_string(),
            rule_id: crate::text::truncate(&f.rule_id, 100),
            elements: (!f.elements.is_empty()).then(|| {
                f.elements
                    .iter()
                    .take(5)
                    .map(|e| crate::text::truncate(e, 120))
                    .collect()
            }),
        })
        .collect();
    let ids = |v: &[super::a11y_sweep::Verdicts]| {
        v.iter()
            .map(|x| IdPass {
                id: x.id.clone(),
                pass: x.pass.clone(),
            })
            .collect()
    };
    Outcome::Done {
        result: A11yResult {
            day: input.job.day.clone(),
            routes: result.routes,
            rules: ids(&result.rules),
            checks: ids(&result.checks),
            ms,
            failures,
            note,
        },
        known: input.known.clone(),
    }
}

#[cfg(test)]
mod tests;
