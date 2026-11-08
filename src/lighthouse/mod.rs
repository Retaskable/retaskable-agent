//! The pure part of the daily Lighthouse chore (docs/specs/daily-lighthouse.md sections 2, 3 and 5;
//! Retaskable.com's `agent/src/lighthouse.ts`): which pages to run, a saved Lighthouse JSON turned into
//! scores and findings, a finding's key, and the day's store lock. No I/O.

use serde_json::Value;

use crate::config::ProjectConfig;

#[cfg(test)]
mod tests;

/// An audit under this score fails.
const FAIL_BELOW: f64 = 0.9;
/// Most offending items kept with a finding.
const MAX_ITEMS: usize = 3;
/// Informative audits that always score 1 and name what is missing as `High` severity items.
const SEVERE_AUDITS: [&str; 4] = ["csp-xss", "has-hsts", "origin-isolation", "trusted-types-xss"];

/// What the chore runs on (`lighthouseTarget`).
#[derive(Debug, Clone, PartialEq)]
pub enum LighthouseTarget {
    /// The chore can't run, and says why.
    Note(String),
    Run {
        url: String,
        pages: Vec<String>,
    },
}

/// `None` when the setting is off; a note when there is no staging url.
pub fn lighthouse_target(cfg: &ProjectConfig) -> Option<LighthouseTarget> {
    if !cfg.lighthouse.enabled {
        return None;
    }
    let url = cfg
        .deploy
        .staging
        .as_ref()
        .and_then(|s| s.url.as_deref())
        .map(str::trim)
        .unwrap_or("");
    if url.is_empty() {
        return Some(LighthouseTarget::Note(
            "Lighthouse needs a staging url (deploy.staging.url) to run on".to_string(),
        ));
    }
    Some(LighthouseTarget::Run {
        url: url.to_string(),
        pages: cfg.lighthouse.pages.clone(),
    })
}

/// The store's chore lock for a day (`yyyy-mm-dd`).
pub fn lighthouse_lock(day: &str) -> String {
    format!("lighthouse-{day}")
}

/// The four category scores, 0 to 100.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Scores {
    pub performance: Option<u32>,
    pub accessibility: Option<u32>,
    pub best_practices: Option<u32>,
    pub seo: Option<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    /// `<page> <preset> <audit id>`, or `<page> <preset> score <category>`.
    pub key: String,
    pub title: String,
    pub display_value: String,
    /// Up to 3 offending URLs or selectors.
    pub items: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Summary {
    pub page: String,
    pub preset: String,
    pub scores: Scores,
    /// Milliseconds.
    pub lcp: Option<f64>,
    pub fcp: Option<f64>,
    pub tbt: Option<f64>,
    pub cls: Option<f64>,
    pub findings: Vec<Finding>,
}

/// An http(s) URL's `scheme://host[:port]`, lower-cased; `None` for a relative or other kind of URL.
fn origin(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    if !matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https") {
        return None;
    }
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.rsplit('@').next().unwrap_or(host);
    Some(format!(
        "{}://{}",
        scheme.to_ascii_lowercase(),
        host.to_ascii_lowercase()
    ))
}

fn item_text(item: &Value) -> Option<String> {
    let s = item
        .get("url")
        .or_else(|| item.pointer("/source/url"))
        .or_else(|| item.pointer("/node/selector"))?
        .as_str()?;
    (!s.is_empty()).then(|| s.to_string())
}

/// A `High` severity item's description (a string, or an object with a `value`).
fn severe_text(item: &Value) -> Option<String> {
    if item.get("severity")?.as_str()? != "High" {
        return None;
    }
    let d = item.get("description")?;
    d.as_str().or_else(|| d.get("value")?.as_str()).map(str::to_string)
}

fn items_of(audit: &Value) -> &[Value] {
    audit
        .pointer("/details/items")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

fn percent(score: &Value) -> Option<u32> {
    score.as_f64().map(|s| (s * 100.0).round() as u32)
}

fn metric(audits: &Value, id: &str) -> Option<f64> {
    audits.pointer(&format!("/{id}/numericValue"))?.as_f64()
}

fn text(v: &Value, field: &str) -> String {
    v.get(field).and_then(Value::as_str).unwrap_or("").to_string()
}

/// Reads a Lighthouse JSON report into a [`Summary`]. `staging` drops `is-crawlable` and the `seo` score from
/// the findings (staging is blocked from indexing on purpose, section 5); the scores keep SEO.
pub fn summarize(report: &Value, page: &str, preset: &str, min_score: u32, staging: bool) -> Summary {
    let audits = report.get("audits").cloned().unwrap_or(Value::Null);
    let categories = report.get("categories").cloned().unwrap_or(Value::Null);
    let host = ["finalUrl", "requestedUrl"]
        .iter()
        .find_map(|f| origin(report.get(*f)?.as_str()?));
    let score_of = |c: &str| percent(categories.pointer(&format!("/{c}/score")).unwrap_or(&Value::Null));
    let mut out = Summary {
        page: page.to_string(),
        preset: preset.to_string(),
        scores: Scores {
            performance: score_of("performance"),
            accessibility: score_of("accessibility"),
            best_practices: score_of("best-practices"),
            seo: score_of("seo"),
        },
        lcp: metric(&audits, "largest-contentful-paint"),
        fcp: metric(&audits, "first-contentful-paint"),
        tbt: metric(&audits, "total-blocking-time"),
        cls: metric(&audits, "cumulative-layout-shift"),
        findings: Vec::new(),
    };
    for (id, audit) in audits.as_object().into_iter().flatten() {
        if staging && id == "is-crawlable" {
            continue;
        }
        let mode = audit.get("scoreDisplayMode").and_then(Value::as_str).unwrap_or("");
        let failing = audit
            .get("score")
            .and_then(Value::as_f64)
            .is_some_and(|s| s < FAIL_BELOW)
            && !matches!(mode, "informative" | "manual" | "notApplicable");
        let severe: Vec<String> = if mode == "informative" && SEVERE_AUDITS.contains(&id.as_str()) {
            items_of(audit).iter().filter_map(severe_text).collect()
        } else {
            Vec::new()
        };
        let items: Vec<String> = if failing {
            let named: Vec<(Option<String>, String)> = items_of(audit)
                .iter()
                .filter_map(item_text)
                .map(|t| (origin(&t), t))
                .collect();
            let urls: Vec<&Option<String>> = named.iter().map(|(o, _)| o).filter(|o| o.is_some()).collect();
            if !urls.is_empty() && host.is_some() && urls.iter().all(|o| **o != host) {
                continue;
            }
            named.into_iter().map(|(_, t)| t).take(MAX_ITEMS).collect()
        } else if !severe.is_empty() {
            severe.into_iter().take(MAX_ITEMS).collect()
        } else {
            continue;
        };
        out.findings.push(Finding {
            key: format!("{page} {preset} {id}"),
            title: text(audit, "title"),
            display_value: text(audit, "displayValue"),
            items,
        });
    }
    for (id, cat) in categories.as_object().into_iter().flatten() {
        if staging && id == "seo" {
            continue;
        }
        if let Some(score) = percent(cat.get("score").unwrap_or(&Value::Null)).filter(|s| *s < min_score) {
            out.findings.push(Finding {
                key: format!("{page} {preset} score {id}"),
                title: format!("{} score", text(cat, "title")),
                display_value: score.to_string(),
                items: Vec::new(),
            });
        }
    }
    out
}

/// The tag a Lighthouse write-up's request starts with.
pub const TAG: &str = "[Lighthouse]";

const TEMPLATE: &str = include_str!("../../prompts/lighthouse.md");

/// A draft is a Lighthouse write-up when its request starts with [`TAG`]; the drafter runs it on the chore tier.
pub fn is_lighthouse_request(request: &str) -> bool {
    request.trim_start().starts_with(TAG)
}

/// Days a discarded draft keeps its findings quiet.
const QUIET_DAYS: i64 = 30;
const DAY_MS: i64 = 86_400_000;

/// What the store says of the draft a known key went into.
#[derive(Debug, Clone, PartialEq)]
pub struct DraftState {
    /// `open`, `merged`, `discarded` or `failed` (anything else counts as open).
    pub status: String,
    /// The draft's last timeline entry, in the store's milliseconds.
    pub last_entry_ms: i64,
}

/// Known keys, each with its draft's state (`None`: the draft is missing).
pub type Known = std::collections::HashMap<String, Option<DraftState>>;

/// `isNewFinding`: an unknown key is new; a known one is quiet while its draft is open or merged, or discarded under
/// 30 days ago (from its last timeline entry, `now` in the store's time). A failed or missing draft's findings are new.
pub fn is_new_finding(key: &str, known: &Known, now: i64) -> bool {
    match known.get(key) {
        None | Some(None) => true,
        Some(Some(d)) => match d.status.as_str() {
            "failed" => true,
            "discarded" => now - d.last_entry_ms >= QUIET_DAYS * DAY_MS,
            _ => false,
        },
    }
}

/// `newFindings`: each key once, only if new, with its page, preset and scores' summary kept alongside.
pub fn new_findings<'a>(summaries: &'a [Summary], known: &Known, now: i64) -> Vec<(&'a Summary, &'a Finding)> {
    let mut seen = std::collections::HashSet::new();
    summaries
        .iter()
        .flat_map(|s| s.findings.iter().map(move |f| (s, f)))
        .filter(|(_, f)| is_new_finding(&f.key, known, now) && seen.insert(f.key.as_str()))
        .collect()
}

fn score(s: Option<u32>) -> String {
    s.map_or("-".into(), |v| v.to_string())
}

/// `lighthouseRequest`: the `[Lighthouse]` draft for the day's new findings, noting the keys it covers.
pub fn lighthouse_request(
    summaries: &[Summary],
    new: &[(&Summary, &Finding)],
    specs_index: &str,
) -> anyhow::Result<crate::backlog::asks::QueuedDraft> {
    let pages: String = summaries
        .iter()
        .map(|s| {
            let c = &s.scores;
            format!(
                "- {} ({}): performance {}, accessibility {}, best practices {}, SEO {}\n",
                s.page,
                s.preset,
                score(c.performance),
                score(c.accessibility),
                score(c.best_practices),
                score(c.seo)
            )
        })
        .collect();
    let findings: String = new
        .iter()
        .map(|(_, f)| {
            let mut t = format!("- `{}`: {}", f.key, f.title);
            if !f.display_value.is_empty() {
                t.push_str(&format!(" ({})", f.display_value));
            }
            t.push('\n');
            for i in &f.items {
                t.push_str(&format!("  - {i}\n"));
            }
            t
        })
        .collect();
    let prompt = crate::prompt::render_prompt(
        TEMPLATE,
        &[
            ("pages", pages),
            ("findings", findings),
            ("specsIndex", specs_index.to_string()),
        ],
    )?;
    let keys: Vec<&str> = new.iter().map(|(_, f)| f.key.as_str()).collect();
    Ok(crate::backlog::asks::QueuedDraft {
        request: format!("{TAG} {prompt}"),
        titles: vec![format!("{TAG} Fix what Lighthouse found on staging")],
        intake: Some(
            serde_json::json!({ "note": format!("Lighthouse finding keys: {}", keys.join("; ")), "keys": keys }),
        ),
        ..Default::default()
    })
}
