//! A Lighthouse report as the job sees it (`summarize` and `rawPages` in Retaskable.com's `agent/src/lighthouse.ts`
//! and `lighthouseChore.ts`): the four scores, LCP, FCP, TBT and CLS, whether each security audit passed, the
//! accessibility audits that applied, and the findings (each failing audit, and each category under the minimum).
//! Pure. The store makes
//! that write-up now, so this is what the `lighthouse` task sends.
use std::collections::BTreeMap;
use std::fmt;

use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::lighthouse::run::Preset;

/// An audit scoring under this fails.
const AUDIT_PASS: f64 = 0.9;
/// Most offending items kept with a finding.
const MAX_ITEMS: usize = 3;
/// Not findings, whatever their score.
const NOT_FINDINGS: [&str; 4] = ["informative", "manual", "notApplicable", "error"];
/// The security headers always score 1 and say what is wrong through a `High` severity item.
const SEVERE: &str = "High";
/// Staging's `robots.txt` blocks indexing on purpose: what that fails isn't a finding there.
const STAGING_BLOCKED: [&str; 1] = ["is-crawlable"];
/// `SECURITY_AUDITS`.
pub const SECURITY_AUDITS: [&str; 7] = [
    "is-on-https",
    "has-hsts",
    "csp-xss",
    "origin-isolation",
    "trusted-types-xss",
    "geolocation-on-start",
    "notification-on-start",
];
const CATEGORIES: [&str; 4] = ["performance", "accessibility", "best-practices", "seo"];

/// A JSON object read in the order the file has it: Lighthouse's audits come in its own order, and the
/// findings (capped, deduplicated) must too.
#[derive(Debug, Default, Clone)]
pub struct Ordered(pub Vec<(String, Value)>);

impl<'de> Deserialize<'de> for Ordered {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Ordered;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("an object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<Ordered, A::Error> {
                let mut out = Vec::new();
                while let Some(kv) = m.next_entry::<String, Value>()? {
                    out.push(kv);
                }
                Ok(Ordered(out))
            }
        }
        d.deserialize_map(V)
    }
}

impl Ordered {
    fn get(&self, key: &str) -> Option<&Value> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
}

/// The parts of a Lighthouse JSON report the job reads.
#[derive(Debug, Default, Deserialize)]
pub struct Report {
    #[serde(default)]
    categories: Option<Ordered>,
    #[serde(default)]
    audits: Option<Ordered>,
}

impl Report {
    /// A report's text; one that isn't JSON reads as an empty report, as `summarize(undefined)` does.
    pub fn parse(text: &str) -> Report {
        serde_json::from_str(text).unwrap_or_default()
    }

    fn audit(&self, id: &str) -> Option<&Value> {
        self.audits.as_ref()?.get(id)
    }

    fn category(&self, id: &str) -> Option<&Value> {
        self.categories.as_ref()?.get(id)
    }
}

/// Each category's score out of 100, or `None` when Lighthouse couldn't score it.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Scores {
    pub performance: Option<u32>,
    pub accessibility: Option<u32>,
    #[serde(rename = "best-practices")]
    pub best_practices: Option<u32>,
    pub seo: Option<u32>,
}

impl Scores {
    fn of(&self, category: &str) -> Option<u32> {
        match category {
            "performance" => self.performance,
            "accessibility" => self.accessibility,
            "best-practices" => self.best_practices,
            _ => self.seo,
        }
    }
}

/// One failing audit or category under the minimum.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Finding {
    /// `<page> <preset> <audit id>`, or `<page> <preset> score <category>`.
    pub key: String,
    pub page: String,
    pub preset: Preset,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_value: Option<String>,
    /// Up to 3 offending URLs or selectors.
    pub items: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct A11yAudit {
    pub id: String,
    pub title: String,
    pub pass: bool,
}

/// The accessibility audits that applied, and how many are left to a person.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct A11ySummary {
    pub audits: Vec<A11yAudit>,
    pub manual: u32,
}

/// One page and preset's report as the chore sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct PageSummary {
    pub page: String,
    pub preset: Preset,
    pub scores: Scores,
    pub lcp_ms: Option<i64>,
    pub fcp_ms: Option<i64>,
    pub tbt_ms: Option<i64>,
    pub cls: Option<f64>,
    pub a11y: Option<A11ySummary>,
    /// Each of [`SECURITY_AUDITS`] the report has: whether it passed.
    pub security: BTreeMap<String, bool>,
    pub findings: Vec<Finding>,
}

/// What a page's summary is run with.
pub struct SummaryOptions<'a> {
    pub page: &'a str,
    pub preset: Preset,
    /// The staging url's `host` (with a port when it has one).
    pub host: &'a str,
    pub min_score: u32,
    pub staging: bool,
}

/// An audit item's URL or selector, if it names one.
fn item_text(item: &Value) -> Option<String> {
    [
        item.get("url"),
        item.pointer("/source/url"),
        item.pointer("/node/selector"),
        item.get("selector"),
    ]
    .into_iter()
    .flatten()
    .filter_map(Value::as_str)
    .map(str::trim)
    .find(|s| !s.is_empty())
    .map(str::to_string)
}

fn items_of(audit: &Value) -> &[Value] {
    audit
        .pointer("/details/items")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

fn mode_of(audit: &Value) -> &str {
    audit.get("scoreDisplayMode").and_then(Value::as_str).unwrap_or("")
}

/// An informative audit's severe problems, in its own words ("No HSTS header found"); none for any other audit.
fn severe_items(audit: &Value) -> Vec<String> {
    if mode_of(audit) != "informative" {
        return Vec::new();
    }
    items_of(audit)
        .iter()
        .filter(|i| i.get("severity").and_then(Value::as_str) == Some(SEVERE))
        .filter_map(|i| i.get("description")?.as_str())
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(str::to_string)
        .collect()
}

fn failing(audit: &Value) -> bool {
    audit
        .get("score")
        .and_then(Value::as_f64)
        .is_some_and(|s| s < AUDIT_PASS)
        && !NOT_FINDINGS.contains(&mode_of(audit))
}

fn is_url(s: &str) -> bool {
    let l = s.to_ascii_lowercase();
    l.starts_with("http://") || l.starts_with("https://")
}

/// `new URL(u).host`: lower-case host, with the port when it isn't the scheme's own; empty when `u` isn't a url.
pub fn host_of(u: &str) -> String {
    let Ok(url) = reqwest::Url::parse(u) else {
        return String::new();
    };
    match (url.host_str(), url.port()) {
        (Some(h), Some(p)) => format!("{h}:{p}"),
        (Some(h), None) => h.to_string(),
        _ => String::new(),
    }
}

fn unique_first(items: impl IntoIterator<Item = String>, n: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for i in items {
        if !out.contains(&i) {
            out.push(i);
        }
    }
    out.truncate(n);
    out
}

fn percent(v: Option<&Value>) -> Option<u32> {
    // `Math.round(s * 100)`: halves go up.
    v?.as_f64().map(|s| (s * 100.0 + 0.5).floor().max(0.0) as u32)
}

fn title_of(v: &Value, fallback: &str) -> String {
    v.get("title")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
        .unwrap_or(fallback)
        .to_string()
}

/// A Lighthouse report as the chore sees it. Offending items on another origin are dropped, and so is a finding
/// left with none, since we can't fix other origins' scripts. On `staging`, `is-crawlable` and the SEO score
/// aren't findings either, since staging blocks indexing on purpose; the scores still have it.
pub fn summarize(report: &Report, o: &SummaryOptions<'_>) -> PageSummary {
    let key = |tail: &str| format!("{} {} {tail}", o.page, o.preset.name());
    let mut scores = Scores::default();
    let mut findings = Vec::new();
    for c in CATEGORIES {
        let cat = report.category(c);
        let score = percent(cat.and_then(|v| v.get("score")));
        match c {
            "performance" => scores.performance = score,
            "accessibility" => scores.accessibility = score,
            "best-practices" => scores.best_practices = score,
            _ => scores.seo = score,
        }
        if let Some(s) = scores.of(c).filter(|s| *s < o.min_score)
            && !(o.staging && c == "seo")
        {
            findings.push(Finding {
                key: key(&format!("score {c}")),
                page: o.page.to_string(),
                preset: o.preset,
                title: format!("{} score {s}", title_of(cat.unwrap_or(&Value::Null), c)),
                display_value: None,
                items: Vec::new(),
            });
        }
    }
    for (id, a) in report.audits.iter().flat_map(|m| m.0.iter()) {
        let severe = severe_items(a);
        if !severe.is_empty() {
            findings.push(Finding {
                key: key(id),
                page: o.page.to_string(),
                preset: o.preset,
                title: title_of(a, id),
                display_value: None,
                items: unique_first(severe, MAX_ITEMS),
            });
            continue;
        }
        if !failing(a) || (o.staging && STAGING_BLOCKED.contains(&id.as_str())) {
            continue;
        }
        let named: Vec<String> = items_of(a).iter().filter_map(item_text).collect();
        let ours: Vec<String> = named
            .iter()
            .filter(|t| !is_url(t) || host_of(t) == o.host)
            .cloned()
            .collect();
        if !named.is_empty() && ours.is_empty() {
            continue;
        }
        findings.push(Finding {
            key: key(id),
            page: o.page.to_string(),
            preset: o.preset,
            title: title_of(a, id),
            display_value: a
                .get("displayValue")
                .and_then(Value::as_str)
                .filter(|d| !d.is_empty())
                .map(str::to_string),
            items: unique_first(ours, MAX_ITEMS),
        });
    }
    let num = |id: &str| report.audit(id)?.get("numericValue")?.as_f64();
    let ms = |id: &str| num(id).map(|v| (v + 0.5).floor() as i64);
    let mut security = BTreeMap::new();
    for id in SECURITY_AUDITS {
        if let Some(a) = report.audit(id) {
            security.insert(id.to_string(), severe_items(a).is_empty() && !failing(a));
        }
    }
    PageSummary {
        page: o.page.to_string(),
        preset: o.preset,
        scores,
        lcp_ms: ms("largest-contentful-paint"),
        fcp_ms: ms("first-contentful-paint"),
        tbt_ms: ms("total-blocking-time"),
        cls: num("cumulative-layout-shift").map(|v| (v * 1000.0 + 0.5).floor() / 1000.0),
        a11y: accessibility_audits(report),
        security,
        findings,
    }
}

/// The accessibility category's audits: each scored pass or fail one (a binary audit, passing at 1), and how many
/// are manual. Ones that didn't apply to the page, or errored, are left out. `None` when the category lists none.
fn accessibility_audits(r: &Report) -> Option<A11ySummary> {
    let refs = r
        .category("accessibility")?
        .get("auditRefs")?
        .as_array()
        .filter(|a| !a.is_empty())?;
    let mut audits = Vec::new();
    let mut manual = 0;
    for rf in refs {
        let Some(id) = rf.get("id").and_then(Value::as_str).filter(|i| !i.is_empty()) else {
            continue;
        };
        let Some(a) = r.audit(id) else {
            continue;
        };
        if mode_of(a) == "manual" {
            manual += 1;
        } else if mode_of(a) == "binary"
            && let Some(score) = a.get("score").and_then(Value::as_f64)
        {
            audits.push(A11yAudit {
                id: id.to_string(),
                title: title_of(a, id),
                pass: score == 1.0,
            });
        }
    }
    Some(A11ySummary { audits, manual })
}

/// One page and preset's measures, audits and security audits in a day's raw result (`rawPages`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RawPage {
    pub page: String,
    pub preset: Preset,
    pub scores: Scores,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lcp_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fcp_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tbt_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cls: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub a11y: Option<A11ySummary>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub security: BTreeMap<String, bool>,
}

/// The pages of a day's raw result.
pub fn raw_pages(summaries: &[PageSummary]) -> Vec<RawPage> {
    summaries
        .iter()
        .map(|s| RawPage {
            page: s.page.clone(),
            preset: s.preset,
            scores: s.scores.clone(),
            lcp_ms: s.lcp_ms,
            fcp_ms: s.fcp_ms,
            tbt_ms: s.tbt_ms,
            cls: s.cls,
            a11y: s.a11y.clone(),
            security: s.security.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests;
