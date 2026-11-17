//! The sweep's checks axe can't make (compliance-scan.md section 3.6; `a11yChecks.ts`): the plain data a page's
//! collectors return and the pure judges over it, and the Tab walk's loop (section 3.6.2), which drives a
//! [`WalkPage`]. The collectors themselves run inside the page (JavaScript through a browser driver) and are not
//! here: whoever builds the real driver supplies them, returning these types.
use std::collections::HashSet;
use std::future::Future;

/// The walk stops at this many presses (section 3.6.2).
pub const MAX_PRESSES: usize = 60;
/// A trap: the last this many presses visited at most [`TRAP_ELEMENTS`] elements, none of them the first.
const TRAP_PRESSES: usize = 10;
const TRAP_ELEMENTS: usize = 3;
/// How many elements a spacing failure names.
const NAMED: usize = 5;

pub const KEYBOARD_UNREACHED: &str = "scan-keyboard-unreached";
pub const KEYBOARD_TRAP: &str = "scan-keyboard-trap";
pub const FOCUS_VISIBLE: &str = "scan-focus-visible";
pub const FOCUS_NOT_HIDDEN: &str = "scan-focus-not-hidden";
pub const REFLOW: &str = "scan-reflow";
pub const TEXT_SPACING: &str = "scan-text-spacing";
pub const PAGE_TITLE: &str = "scan-page-title";

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Boxed {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// The computed styles section 3.6.3 compares.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FocusStyles {
    pub outline_style: String,
    pub outline_width: String,
    pub outline_color: String,
    pub box_shadow: String,
    pub border_color: String,
    pub background_color: String,
}

/// One `elementFromPoint` answer: whether it is the element or inside it, and what it is if not.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub on_element: bool,
    pub role: String,
    pub name: String,
}

/// One Tab press. `element` is the number the page gave it (the same element always has the same number), `None`
/// for the page.
#[derive(Debug, Clone, PartialEq)]
pub struct WalkStep {
    pub element: Option<i64>,
    pub role: String,
    /// Trimmed to 60 characters by the collector.
    pub name: String,
    pub boxed: Boxed,
    /// Whether the box is in the viewport after the browser scrolled it into view.
    pub in_viewport: bool,
    pub focused: FocusStyles,
    pub unfocused: FocusStyles,
    /// The centre and 2 px in from each corner; `None` where nothing answered.
    pub hits: Vec<Option<Hit>>,
}

/// What a keyboard user must be able to reach, in document order; `element` numbers as the walk's do.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub element: i64,
    pub role: String,
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Walk {
    pub steps: Vec<WalkStep>,
    pub candidates: Vec<Candidate>,
    /// After Escape and one more Tab at a stuck or circling walk: the element then focused (`Some(None)`: the
    /// page). `None` when not tried.
    pub escape: Option<Option<i64>>,
}

/// An element whose own text overflows a box that hides it.
#[derive(Debug, Clone, PartialEq)]
pub struct Overflow {
    pub element: i64,
    pub role: String,
    pub name: String,
    pub ellipsis: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReflowRoute {
    pub route: String,
    pub scroll_width: f64,
    pub client_width: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SpacingRoute {
    pub route: String,
    pub before: Vec<Overflow>,
    pub after: Vec<Overflow>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RouteTitle {
    pub route: String,
    pub title: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Failure {
    pub detail: String,
    /// Elements by role and accessible name, never a selector.
    pub elements: Option<Vec<String>>,
    /// How many more elements than `elements` names.
    pub more: Option<usize>,
    pub route: Option<String>,
    pub routes: Option<Vec<String>>,
}

impl Failure {
    fn on(detail: &str, element: String) -> Failure {
        Failure {
            detail: detail.to_string(),
            elements: Some(vec![element]),
            ..Default::default()
        }
    }
}

/// A check's verdict on what the page collected.
#[derive(Debug, Clone, PartialEq)]
pub struct Judged {
    pub id: &'static str,
    pub pass: bool,
    pub failures: Vec<Failure>,
}

fn result(id: &'static str, failures: Vec<Failure>) -> Judged {
    Judged {
        id,
        pass: failures.is_empty(),
        failures,
    }
}

fn describe(role: &str, name: &str) -> String {
    if name.is_empty() {
        role.to_string()
    } else {
        format!("{role} '{name}'")
    }
}

fn is_empty(b: &Boxed) -> bool {
    b.width == 0.0 || b.height == 0.0
}

/// The steps that reached an element, each element once, at its first press.
fn reached(steps: &[WalkStep]) -> Vec<&WalkStep> {
    let mut seen = HashSet::new();
    steps
        .iter()
        .filter(|s| s.element.is_some_and(|e| seen.insert(e)))
        .collect()
}

/// The elements of a trap, if the presses show one: the same element twice in a row, or the last 10 on 3 or
/// fewer, none the first.
pub fn trapped(steps: &[WalkStep]) -> Option<HashSet<i64>> {
    let first = steps.iter().find_map(|s| s.element);
    for i in 1..steps.len() {
        if let Some(e) = steps[i].element
            && steps[i].element == steps[i - 1].element
        {
            return Some(HashSet::from([e]));
        }
        if i + 1 < TRAP_PRESSES {
            continue;
        }
        let window: HashSet<Option<i64>> = steps[i + 1 - TRAP_PRESSES..=i].iter().map(|s| s.element).collect();
        if window.len() <= TRAP_ELEMENTS && !window.contains(&first) && !window.contains(&None) {
            return Some(window.into_iter().flatten().collect());
        }
    }
    None
}

/// 2.1.1 and 2.1.2 (sections 3.6.2 and 3.6.4): elements Tab never reached, and focus that stops or circles.
pub fn judge_walk(walk: &Walk) -> Vec<Judged> {
    let (steps, candidates) = (&walk.steps, &walk.candidates);
    let touched = reached(steps);
    let trap = trapped(steps);
    // Focus leaving for the page (`Some(None)`) is an escape; so is an element outside the trap.
    let escaped = match (&trap, walk.escape) {
        (Some(t), Some(e)) => e.is_none_or(|e| !t.contains(&e)),
        _ => false,
    };
    let mut trap_failures = Vec::new();
    if let Some(t) = &trap
        && !escaped
    {
        let named: Vec<String> = touched
            .iter()
            .filter(|s| s.element.is_some_and(|e| t.contains(&e)))
            .map(|s| describe(&s.role, &s.name))
            .collect();
        trap_failures.push(Failure {
            detail: "focus cannot leave these elements with Tab or Escape".to_string(),
            elements: Some(named),
            ..Default::default()
        });
    }
    // A round (back on the first element) or focus leaving for the page is a whole walk; a cut or a trap judges
    // only up to the last element reached.
    let first = steps.iter().find_map(|s| s.element);
    let whole = trap.is_none()
        && steps
            .last()
            .is_some_and(|last| last.element.is_none() || (steps.len() > 1 && last.element == first));
    let got: HashSet<i64> = touched.iter().filter_map(|s| s.element).collect();
    let up_to = if whole {
        candidates.len()
    } else {
        candidates.iter().rposition(|c| got.contains(&c.element)).unwrap_or(0)
    };
    let mut failures: Vec<Failure> = candidates
        .iter()
        .take(up_to)
        .filter(|c| !got.contains(&c.element))
        .map(|c| Failure::on("Tab never reaches it", describe(&c.role, &c.name)))
        .collect();
    for s in touched.iter().filter(|t| is_empty(&t.boxed)) {
        failures.push(Failure::on("focusable but not drawn (0×0)", describe(&s.role, &s.name)));
    }
    vec![
        result(KEYBOARD_UNREACHED, failures),
        result(KEYBOARD_TRAP, trap_failures),
    ]
}

/// 2.4.7 (section 3.6.3): passes an element when any compared style differs between focused and not.
pub fn judge_focus_styles(steps: &[WalkStep]) -> Judged {
    let failures = reached(steps)
        .into_iter()
        .filter(|s| !is_empty(&s.boxed) && s.focused == s.unfocused)
        .map(|s| Failure::on("nothing shows it has focus", describe(&s.role, &s.name)))
        .collect();
    result(FOCUS_VISIBLE, failures)
}

/// 2.4.11 (section 3.6.5): fails only when none of the five points lands on the element.
pub fn judge_obscured(steps: &[WalkStep]) -> Judged {
    let mut failures = Vec::new();
    for s in reached(steps) {
        if is_empty(&s.boxed) || !s.in_viewport || s.hits.iter().flatten().any(|h| h.on_element) {
            continue;
        }
        let cover = s.hits.iter().flatten().find(|h| !h.on_element);
        let detail = cover.map_or("covered".to_string(), |c| {
            format!("under {}", describe(&c.role, &c.name))
        });
        failures.push(Failure {
            detail,
            elements: Some(vec![describe(&s.role, &s.name)]),
            ..Default::default()
        });
    }
    result(FOCUS_NOT_HIDDEN, failures)
}

/// 1.4.10 (section 3.6.6): the document itself scrolls sideways.
pub fn judge_reflow(routes: &[ReflowRoute]) -> Judged {
    let failures = routes
        .iter()
        .filter(|r| r.scroll_width > r.client_width + 1.0)
        .map(|r| Failure {
            route: Some(r.route.clone()),
            detail: format!("needs {} px at {}", r.scroll_width, r.client_width),
            ..Default::default()
        })
        .collect();
    result(REFLOW, failures)
}

/// 1.4.12 (section 3.6.7): elements that overflow only once spaced, ellipsis and line-clamp left out.
pub fn judge_spacing(routes: &[SpacingRoute]) -> Judged {
    let mut failures = Vec::new();
    for r in routes {
        let before: HashSet<i64> = r.before.iter().map(|o| o.element).collect();
        let fresh: Vec<&Overflow> = r
            .after
            .iter()
            .filter(|o| !before.contains(&o.element) && !o.ellipsis)
            .collect();
        if fresh.is_empty() {
            continue;
        }
        failures.push(Failure {
            route: Some(r.route.clone()),
            detail: "text spills out of its box when spaced".to_string(),
            elements: Some(fresh.iter().take(NAMED).map(|o| describe(&o.role, &o.name)).collect()),
            more: Some(fresh.len().saturating_sub(NAMED)),
            ..Default::default()
        });
    }
    result(TEXT_SPACING, failures)
}

/// A route without its query or fragment.
pub fn path_of(route: &str) -> &str {
    route.split(['?', '#']).next().unwrap_or("")
}

/// 2.4.2 (section 3.6.8): an empty title fails its route; routes sharing one title are one finding.
pub fn judge_titles(routes: &[RouteTitle]) -> Judged {
    let mut by_path: Vec<(&str, &str)> = Vec::new();
    for r in routes {
        if !by_path.iter().any(|(p, _)| *p == path_of(&r.route)) {
            by_path.push((path_of(&r.route), r.title.trim()));
        }
    }
    let mut failures = Vec::new();
    let mut by_title: Vec<(&str, Vec<String>)> = Vec::new();
    for (path, title) in by_path {
        if title.is_empty() {
            failures.push(Failure {
                route: Some(path.to_string()),
                detail: "the page has no title".to_string(),
                ..Default::default()
            });
        } else if let Some((_, paths)) = by_title.iter_mut().find(|(t, _)| *t == title) {
            paths.push(path.to_string());
        } else {
            by_title.push((title, vec![path.to_string()]));
        }
    }
    for (title, paths) in by_title {
        if paths.len() > 1 {
            failures.push(Failure {
                detail: format!("{} routes share the title {title}", paths.len()),
                routes: Some(paths),
                ..Default::default()
            });
        }
    }
    result(PAGE_TITLE, failures)
}

/// One press and what the page said of it: the step, and the styles the previous element has now it has lost focus.
#[derive(Debug, Clone, PartialEq)]
pub struct Pressed {
    pub step: WalkStep,
    pub prev: Option<FocusStyles>,
}

/// A page the Tab walk drives: a desktop, light page of its own, loaded and blurred. `press` is the key, then a
/// settle (the Sample's fake clock 50 ms, elsewhere two animation frames), then the focused element's collector.
pub trait WalkPage {
    fn candidates(&mut self) -> impl Future<Output = Result<Vec<Candidate>, String>> + Send;
    fn press_tab(&mut self) -> impl Future<Output = Result<Pressed, String>> + Send;
    /// Escape (and its settle), then one more Tab.
    fn press_escape_then_tab(&mut self) -> impl Future<Output = Result<Pressed, String>> + Send;
    /// Blurs the focused element and reads its styles.
    fn blur_and_read(&mut self) -> impl Future<Output = Result<Option<FocusStyles>, String>> + Send;
}

/// The Tab walk of section 3.6.2 on a loaded page: up to 60 presses, Escape and one more Tab at a suspected trap;
/// then the four checks' verdicts. An empty list when the walk reached nothing, so the checks are left out rather
/// than passed.
pub async fn walk_checks<P: WalkPage>(page: &mut P) -> Result<Vec<Judged>, String> {
    let candidates = page.candidates().await?;
    let mut walk = Walk {
        candidates,
        ..Default::default()
    };
    let mut first: Option<i64> = None;
    let mut stuck = false;
    while walk.steps.len() < MAX_PRESSES {
        let Pressed { step, prev } = page.press_tab().await?;
        if let (Some(prev), Some(last)) = (prev, walk.steps.last_mut()) {
            last.unfocused = prev;
        }
        let element = step.element;
        let mut s = step;
        s.unfocused = s.focused.clone();
        walk.steps.push(s);
        if trapped(&walk.steps).is_some() {
            let after = page.press_escape_then_tab().await?;
            walk.escape = Some(after.step.element);
            if let (Some(prev), Some(last)) = (after.prev, walk.steps.last_mut()) {
                last.unfocused = prev;
            }
            stuck = true;
            break;
        }
        match (element, first) {
            (None, _) => break,
            (Some(e), None) => first = Some(e),
            (Some(e), Some(f)) if e == f => break,
            _ => {}
        }
    }
    let last = page.blur_and_read().await?;
    if let (Some(last), false) = (last, stuck) {
        // The last press that landed on an element: its unfocused styles are those read after the final blur.
        if let Some(s) = walk.steps.iter_mut().rev().find(|s| s.element.is_some()) {
            s.unfocused = last;
        }
    }
    if !walk.steps.iter().any(|s| s.element.is_some()) {
        return Ok(Vec::new());
    }
    let mut out = judge_walk(&walk);
    out.push(judge_focus_styles(&walk.steps));
    out.push(judge_obscured(&walk.steps));
    Ok(out)
}

#[cfg(test)]
mod tests;
