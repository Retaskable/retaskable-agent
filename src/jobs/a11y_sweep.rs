//! The accessibility sweep itself (compliance-scan.md section 3.3; `a11yScan.ts`): axe-core against the site's routes
//! at phone and desktop size, light and dark, through a headless Chrome. The aggregation into the store's shape
//! ([`sweep_routes`]) is pure given a [`RouteRunner`]; [`real_sweep`] drives a [`BrowserLauncher`]'s browser. No
//! browser driver is built in this crate yet: a driver implements [`SweepBrowser`] (axe-core run in the page, the
//! collectors of `a11yChecks.ts` and `a11yPage.ts`) and [`BrowserLauncher`].
use std::future::Future;
use std::sync::Mutex;
use std::time::Instant;

use serde_json::Value;

use super::a11y_judge::{
    Judged, Overflow, PAGE_TITLE, ReflowRoute, RouteTitle, SpacingRoute, WalkPage, judge_reflow, judge_spacing,
    judge_titles, path_of, walk_checks,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Size {
    Phone,
    Desktop,
}

impl Size {
    pub fn name(self) -> &'static str {
        match self {
            Size::Phone => "phone",
            Size::Desktop => "desktop",
        }
    }

    /// Phone and desktop sizes (compliance-scan.md section 3.3).
    pub fn viewport(self) -> (u32, u32) {
        match self {
            Size::Phone => (390, 844),
            Size::Desktop => (1440, 900),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Theme {
    Light,
    Dark,
}

impl Theme {
    pub fn name(self) -> &'static str {
        match self {
            Theme::Light => "light",
            Theme::Dark => "dark",
        }
    }
}

const FULL_TAGS: [&str; 6] = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa", "best-practice"];
const CONTRAST_ONLY: [&str; 1] = ["color-contrast"];
const DESKTOP_RULES: [&str; 2] = ["color-contrast", "target-size"];

/// What axe-core runs in a pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOnly {
    /// `runOnly: { type: "tag" }`: the full phone-light pass.
    Tags(&'static [&'static str]),
    /// `runOnly: { type: "rule" }`: every other pass, to keep the sweep near a minute.
    Rules(&'static [&'static str]),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pass {
    pub size: Size,
    pub theme: Theme,
    pub run_only: RunOnly,
}

/// Phone light runs every rule; the rest only check what a theme or a size changes (section 3.3).
pub const PASSES: [Pass; 4] = [
    Pass {
        size: Size::Phone,
        theme: Theme::Light,
        run_only: RunOnly::Tags(&FULL_TAGS),
    },
    Pass {
        size: Size::Phone,
        theme: Theme::Dark,
        run_only: RunOnly::Rules(&CONTRAST_ONLY),
    },
    Pass {
        size: Size::Desktop,
        theme: Theme::Light,
        run_only: RunOnly::Rules(&DESKTOP_RULES),
    },
    Pass {
        size: Size::Desktop,
        theme: Theme::Dark,
        run_only: RunOnly::Rules(&DESKTOP_RULES),
    },
];

/// One pass's axe-core result on one route: the rule ids that ran, split pass/fail.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PassResult {
    pub passed: Vec<String>,
    pub failed: Vec<String>,
}

/// The size a check ran at: a [`Size`], or the reflow check's 320x256.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CheckSize {
    Size(Size),
    Reflow,
}

impl CheckSize {
    pub fn name(self) -> &'static str {
        match self {
            CheckSize::Size(s) => s.name(),
            CheckSize::Reflow => "320x256",
        }
    }
}

/// One check's verdict on one route, at the size and theme it ran in (section 3.6.1); `elements` names up to 5 by
/// role and accessible name, never a selector.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckResult {
    pub id: String,
    pub size: CheckSize,
    pub theme: Theme,
    pub pass: bool,
    pub elements: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SweepOccurrence {
    pub route: String,
    pub size: CheckSize,
    pub theme: Theme,
    pub rule_id: String,
    /// For a check's failure: the elements it names, role and name.
    pub elements: Vec<String>,
    /// `a11y <route> <size> <theme> <rule id>` (section 3.3's Keys bullet); never a selector.
    pub key: String,
}

/// One rule or check's pass/fail per route that ran it, in route order.
#[derive(Debug, Clone, PartialEq)]
pub struct Verdicts {
    pub id: String,
    pub pass: Vec<bool>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SweepResult {
    pub routes: usize,
    pub rules: Vec<Verdicts>,
    /// Every occurrence any pass found failing, in route order, for the write-up; never sent to the store as is.
    pub failures: Vec<SweepOccurrence>,
    /// Each check that ran on at least one route.
    pub checks: Vec<Verdicts>,
}

/// What the sweep asks of a browser, route by route.
pub trait RouteRunner {
    fn run_pass(&self, route: &str, pass: &Pass) -> impl Future<Output = Result<PassResult, String>> + Send;
    /// A route's own checks (the Tab walk, reflow, text spacing, the title read).
    fn run_checks(&self, route: &str) -> impl Future<Output = Result<Vec<CheckResult>, String>> + Send;
    /// After every route ran: the checks that compare routes against each other (the page title, section 3.6.8).
    fn cross_route(&self, per_route: &[(String, Vec<CheckResult>)]) -> Vec<(String, CheckResult)>;
}

/// An insertion-ordered map of id to verdicts.
fn push_verdict(map: &mut Vec<Verdicts>, id: &str, ok: bool) {
    match map.iter_mut().find(|v| v.id == id) {
        Some(v) => v.pass.push(ok),
        None => map.push(Verdicts {
            id: id.to_string(),
            pass: vec![ok],
        }),
    }
}

fn occurrence(route: &str, size: CheckSize, theme: Theme, id: &str, elements: Vec<String>) -> SweepOccurrence {
    SweepOccurrence {
        route: route.to_string(),
        size,
        theme,
        rule_id: id.to_string(),
        elements,
        key: format!("a11y {route} {} {} {id}", size.name(), theme.name()),
    }
}

/// Runs every pass on every route and aggregates into the store's shape (`ScanA11yRaw`): one pass/fail per route
/// per rule, AND-merged across the passes that ran it (a rule like `color-contrast` runs in more than one pass). A
/// rule's `pass` only has an entry for a route that ran it at least once, in route order.
pub async fn sweep_routes<R: RouteRunner>(routes: &[String], passes: &[Pass], r: &R) -> Result<SweepResult, String> {
    let mut by_rule: Vec<Verdicts> = Vec::new();
    let mut failures = Vec::new();
    let mut per_route: Vec<(String, Vec<CheckResult>)> = Vec::new();
    for route in routes {
        let mut verdict: Vec<(String, bool)> = Vec::new();
        let mut set = |id: &str, ok: bool| match verdict.iter_mut().find(|(i, _)| i == id) {
            Some((_, v)) => *v = *v && ok,
            None => verdict.push((id.to_string(), ok)),
        };
        for pass in passes {
            let res = r.run_pass(route, pass).await?;
            for id in &res.passed {
                set(id, true);
            }
            for id in &res.failed {
                set(id, false);
                failures.push(occurrence(
                    route,
                    CheckSize::Size(pass.size),
                    pass.theme,
                    id,
                    Vec::new(),
                ));
            }
        }
        for (id, ok) in verdict {
            push_verdict(&mut by_rule, &id, ok);
        }
        per_route.push((route.clone(), r.run_checks(route).await?));
    }
    for (route, result) in r.cross_route(&per_route) {
        if let Some((_, results)) = per_route.iter_mut().find(|(p, _)| *p == route) {
            results.push(result);
        }
    }
    let mut by_check: Vec<Verdicts> = Vec::new();
    for (route, results) in &per_route {
        let mut verdict: Vec<(String, bool)> = Vec::new();
        for c in results {
            match verdict.iter_mut().find(|(i, _)| *i == c.id) {
                Some((_, v)) => *v = *v && c.pass,
                None => verdict.push((c.id.clone(), c.pass)),
            }
            if !c.pass {
                let named = c.elements.iter().take(5).cloned().collect();
                failures.push(occurrence(route, c.size, c.theme, &c.id, named));
            }
        }
        for (id, ok) in verdict {
            push_verdict(&mut by_check, &id, ok);
        }
    }
    Ok(SweepResult {
        routes: routes.len(),
        rules: by_rule,
        failures,
        checks: by_check,
    })
}

/// A page the sweep opens for one size and theme, and the walks, reflow, spacing and title collectors of a route.
/// Implemented by a browser driver.
pub trait SweepBrowser {
    type Walker: WalkPage + Send;
    /// Opens `route` in the page for the pass's size and theme and runs axe-core on it.
    fn run_pass(&self, route: &str, pass: &Pass) -> impl Future<Output = Result<PassResult, String>> + Send;
    /// A desktop, light page of its own on `route`, loaded (the Sample's clock run) and blurred. `sample` is a route
    /// the app serves from its Sample, whose clock is the driver's fake one.
    fn walker(&self, route: &str, sample: bool) -> impl Future<Output = Result<Self::Walker, String>> + Send;
    /// The document's scroll width and client width in a 320x256 viewport.
    fn reflow(&self, route: &str) -> impl Future<Output = Result<(f64, f64), String>> + Send;
    /// The text overflows before and after the spacing styles, at phone size.
    fn spacing(&self, route: &str) -> impl Future<Output = Result<(Vec<Overflow>, Vec<Overflow>), String>> + Send;
    /// The page's `document.title` at desktop size.
    fn title(&self, route: &str) -> impl Future<Output = Result<String, String>> + Send;
}

/// A [`SweepBrowser`] as the sweep's [`RouteRunner`]: the walks, reflow, spacing and titles judged here.
pub struct BrowserRunner<'a, B: SweepBrowser> {
    pub browser: &'a B,
    /// Whether the Sample's prefs are in use (a route with `?sample` then runs on its fake clock).
    pub samples: bool,
    titles: Mutex<Vec<RouteTitle>>,
}

impl<'a, B: SweepBrowser> BrowserRunner<'a, B> {
    pub fn new(browser: &'a B, samples: bool) -> Self {
        BrowserRunner {
            browser,
            samples,
            titles: Mutex::new(Vec::new()),
        }
    }
}

/// A judge's verdict on one route as the sweep carries it; the finding's detail rides in `elements`.
fn carry(j: &Judged, size: CheckSize) -> CheckResult {
    let elements: Vec<String> = j
        .failures
        .iter()
        .flat_map(|f| match &f.elements {
            Some(e) if !e.is_empty() => e.clone(),
            _ => vec![f.detail.clone()],
        })
        .collect();
    CheckResult {
        id: j.id.to_string(),
        size,
        theme: Theme::Light,
        pass: j.pass,
        elements,
    }
}

impl<B: SweepBrowser + Sync> RouteRunner for BrowserRunner<'_, B> {
    fn run_pass(&self, route: &str, pass: &Pass) -> impl Future<Output = Result<PassResult, String>> + Send {
        self.browser.run_pass(route, pass)
    }

    /// The checks (section 3.6): the Tab walk on a desktop light page of its own per route, then reflow, text
    /// spacing and the title read; the titles are judged across routes.
    async fn run_checks(&self, route: &str) -> Result<Vec<CheckResult>, String> {
        let sample = self.samples && route.contains("?sample");
        // A walk that fails leaves its checks out, as `walkChecks(...).catch(() => [])` does.
        let walked = match self.browser.walker(route, sample).await {
            Ok(mut w) => walk_checks(&mut w).await.unwrap_or_default(),
            Err(_) => Vec::new(),
        };
        let mut out: Vec<CheckResult> = walked
            .iter()
            .map(|j| CheckResult {
                id: j.id.to_string(),
                size: CheckSize::Size(Size::Desktop),
                theme: Theme::Light,
                pass: j.pass,
                elements: j
                    .failures
                    .iter()
                    .flat_map(|f| f.elements.clone().unwrap_or_default())
                    .collect(),
            })
            .collect();
        let (scroll_width, client_width) = self.browser.reflow(route).await?;
        let reflow = judge_reflow(&[ReflowRoute {
            route: route.to_string(),
            scroll_width,
            client_width,
        }]);
        out.push(carry(&reflow, CheckSize::Reflow));
        let (before, after) = self.browser.spacing(route).await?;
        let spacing = judge_spacing(&[SpacingRoute {
            route: route.to_string(),
            before,
            after,
        }]);
        out.push(carry(&spacing, CheckSize::Size(Size::Phone)));
        let title = self.browser.title(route).await?;
        self.titles.lock().unwrap().push(RouteTitle {
            route: route.to_string(),
            title,
        });
        Ok(out)
    }

    fn cross_route(&self, _: &[(String, Vec<CheckResult>)]) -> Vec<(String, CheckResult)> {
        let titles = self.titles.lock().unwrap();
        let failing = judge_titles(&titles).failures;
        let bad = |route: &str| -> Vec<&str> {
            failing
                .iter()
                .filter(|f| {
                    f.route
                        .iter()
                        .chain(f.routes.iter().flatten())
                        .any(|p| p == path_of(route))
                })
                .map(|f| f.detail.as_str())
                .collect()
        };
        titles
            .iter()
            .map(|t| {
                let fails = bad(&t.route);
                (
                    t.route.clone(),
                    CheckResult {
                        id: PAGE_TITLE.to_string(),
                        size: CheckSize::Size(Size::Desktop),
                        theme: Theme::Light,
                        pass: fails.is_empty(),
                        elements: fails.into_iter().map(str::to_string).collect(),
                    },
                )
            })
            .collect()
    }
}

/// Why a browser did not start; `no_chrome` when the machine has no Chrome at all.
#[derive(Debug, Clone, PartialEq)]
pub struct LaunchError {
    pub no_chrome: bool,
    pub message: String,
}

/// Starts the headless Chrome with one page per size and theme (the Sample's prefs stored in each when given,
/// `scheme` forced to the page's theme so a saved scheme never wins).
pub trait BrowserLauncher {
    type Browser: SweepBrowser + Send + Sync;
    fn launch(
        &self,
        base: &str,
        sample_prefs: Option<&Value>,
    ) -> impl Future<Output = Result<Self::Browser, LaunchError>> + Send;
}

/// A sweep's end (`SweepRun`).
#[derive(Debug, Clone, PartialEq)]
pub enum SweepRun {
    Ok { result: SweepResult, ms: u64 },
    Failed { no_chrome: bool, error: String },
}

/// What a job needs of a sweep: the real one is [`BrowserSweeper`].
pub trait Sweeper {
    fn sweep(
        &self,
        base: &str,
        routes: &[String],
        sample_prefs: Option<&Value>,
    ) -> impl Future<Output = SweepRun> + Send;
}

/// `realSweep`: launches, sweeps every route with [`PASSES`], and closes (the browser drops).
pub struct BrowserSweeper<L: BrowserLauncher>(pub L);

impl<L: BrowserLauncher + Sync> Sweeper for BrowserSweeper<L> {
    async fn sweep(&self, base: &str, routes: &[String], sample_prefs: Option<&Value>) -> SweepRun {
        let started = Instant::now();
        let browser = match self.0.launch(base, sample_prefs).await {
            Ok(b) => b,
            Err(e) => {
                return SweepRun::Failed {
                    no_chrome: e.no_chrome,
                    error: e.message,
                };
            }
        };
        let runner = BrowserRunner::new(&browser, sample_prefs.is_some());
        match sweep_routes(routes, &PASSES, &runner).await {
            Ok(result) => SweepRun::Ok {
                result,
                ms: started.elapsed().as_millis() as u64,
            },
            Err(error) => SweepRun::Failed {
                no_chrome: false,
                error,
            },
        }
    }
}

#[cfg(test)]
mod tests;
