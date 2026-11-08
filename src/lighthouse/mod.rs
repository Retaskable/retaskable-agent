//! The pure part of the daily Lighthouse chore (docs/specs/daily-lighthouse.md sections 2, 3 and 5; Retaskable.com's
//! `agent/src/lighthouse.ts`): which pages to run ([`lighthouse_target`]), a saved Lighthouse JSON turned into scores,
//! Core Web Vitals and findings ([`summarize`]), and the store's lock name for the day ([`lighthouse_lock`]).
//! No I/O.

use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde_json::Value;

use crate::config::ProjectConfig;

/// An audit under this score fails.
const PASS: f64 = 0.9;
/// How many offending items a finding keeps.
const MAX_ITEMS: usize = 3;
/// Informative audits that report what is missing as `High` severity items while scoring 1.
const SEVERE_AUDITS: [&str; 4] = ["csp-xss", "has-hsts", "origin-isolation", "trusted-types-xss"];
const CATEGORIES: [&str; 4] = ["performance", "accessibility", "best-practices", "seo"];

/// What the chore does for a project with the setting on.
#[derive(Debug, Clone, PartialEq)]
pub enum LighthouseTarget {
    /// Doesn't run, and says why.
    Skip {
        note: String,
    },
    Run {
        staging_url: String,
        pages: Vec<String>,
    },
}

/// `None` when the setting is off; a note without a staging url; else the staging url and the pages.
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
        return Some(LighthouseTarget::Skip {
            note: "Lighthouse needs a staging url (deploy.staging.url) to run on".to_string(),
        });
    }
    Some(LighthouseTarget::Run {
        staging_url: url.trim_end_matches('/').to_string(),
        pages: cfg.lighthouse.pages.clone(),
    })
}

/// The store's chore lock for a day: `lighthouse-<yyyy-mm-dd>`.
pub fn lighthouse_lock(day: NaiveDate) -> String {
    format!("lighthouse-{}", day.format("%Y-%m-%d"))
}

/// The four category scores (0 to 100), `None` when the report lacks one.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
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
    /// Up to 3 offending URLs or selectors (or, for a severe informative audit, its problems).
    pub items: Vec<String>,
}

/// One page on one preset.
#[derive(Debug, Clone, PartialEq)]
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

fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

/// The host of an `http(s)` URL, lowercased with any port; `None` for a selector or a relative path.
fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?;
    (!host.is_empty()).then(|| host.to_lowercase())
}

/// An audit's items as the strings the write-up shows: `url`, `source.url` or `node.selector`.
fn item_names(audit: &Value) -> Vec<String> {
    let items = audit.pointer("/details/items").and_then(Value::as_array);
    items
        .into_iter()
        .flatten()
        .filter_map(|i| {
            i.get("url")
                .or_else(|| i.pointer("/source/url"))
                .or_else(|| i.pointer("/node/selector"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect()
}

/// The `High` severity problems of an informative audit, in its own words.
fn severe_items(audit: &Value) -> Vec<String> {
    let items = audit.pointer("/details/items").and_then(Value::as_array);
    items
        .into_iter()
        .flatten()
        .filter(|i| str_of(i, "severity") == "High")
        .filter_map(|i| {
            let d = i.get("description")?;
            d.as_str()
                .or_else(|| d.get("value").and_then(Value::as_str))
                .map(str::to_string)
        })
        .collect()
}

fn percent(score: f64) -> u32 {
    (score * 100.0).round() as u32
}

/// Reads a Lighthouse JSON report. With `staging`, `is-crawlable` and the `seo` score aren't findings (section 5),
/// though the scores keep SEO. Findings whose named URLs are all on another host than the page's are dropped.
pub fn summarize(report_json: &str, page: &str, preset: &str, min_score: f64, staging: bool) -> Result<Summary> {
    let report: Value = serde_json::from_str(report_json).context("not a Lighthouse report")?;
    let host = host_of(str_of(&report, "requestedUrl")).or_else(|| host_of(str_of(&report, "finalUrl")));
    let audits = report
        .get("audits")
        .and_then(Value::as_object)
        .context("report has no audits")?;
    let numeric = |id: &str| {
        audits
            .get(id)
            .and_then(|a| a.get("numericValue"))
            .and_then(Value::as_f64)
    };

    let category = |id: &str| {
        report
            .pointer(&format!("/categories/{id}/score"))
            .and_then(Value::as_f64)
    };
    let scores = Scores {
        performance: category("performance").map(percent),
        accessibility: category("accessibility").map(percent),
        best_practices: category("best-practices").map(percent),
        seo: category("seo").map(percent),
    };

    let mut findings = Vec::new();
    for (id, audit) in audits {
        if staging && id == "is-crawlable" {
            continue;
        }
        let mode = str_of(audit, "scoreDisplayMode");
        let (items, failing) = if mode == "informative" && SEVERE_AUDITS.contains(&id.as_str()) {
            let severe = severe_items(audit);
            let failing = !severe.is_empty();
            (severe, failing)
        } else {
            let failing = audit.get("score").and_then(Value::as_f64).is_some_and(|s| s < PASS)
                && !matches!(mode, "informative" | "manual" | "notApplicable");
            (item_names(audit), failing)
        };
        if !failing {
            continue;
        }
        let urls: Vec<String> = items.iter().filter_map(|i| host_of(i)).collect();
        if host.is_some() && !urls.is_empty() && urls.iter().all(|u| Some(u) != host.as_ref()) {
            continue;
        }
        findings.push(Finding {
            key: format!("{page} {preset} {id}"),
            title: str_of(audit, "title").to_string(),
            display_value: str_of(audit, "displayValue").to_string(),
            items: items.into_iter().take(MAX_ITEMS).collect(),
        });
    }
    for id in CATEGORIES {
        if staging && id == "seo" {
            continue;
        }
        let Some(score) = category(id).map(percent) else {
            continue;
        };
        if f64::from(score) < min_score {
            let title = report
                .pointer(&format!("/categories/{id}/title"))
                .and_then(Value::as_str)
                .unwrap_or(id);
            findings.push(Finding {
                key: format!("{page} {preset} score {id}"),
                title: format!("{title} score"),
                display_value: score.to_string(),
                items: Vec::new(),
            });
        }
    }

    Ok(Summary {
        page: page.to_string(),
        preset: preset.to_string(),
        scores,
        lcp: numeric("largest-contentful-paint"),
        fcp: numeric("first-contentful-paint"),
        tbt: numeric("total-blocking-time"),
        cls: numeric("cumulative-layout-shift"),
        findings,
    })
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
