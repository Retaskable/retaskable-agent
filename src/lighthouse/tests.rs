use super::*;
use crate::config::{DeployConfig, DeployTarget, LighthouseConfig};
use std::path::PathBuf;

fn report() -> String {
    let path: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "tests",
        "fixtures",
        "lighthouse",
        "report.json",
    ]
    .iter()
    .collect();
    std::fs::read_to_string(path).unwrap()
}

fn keys(s: &Summary) -> Vec<&str> {
    s.findings.iter().map(|f| f.key.as_str()).collect()
}

fn find<'a>(s: &'a Summary, key: &str) -> &'a Finding {
    s.findings
        .iter()
        .find(|f| f.key == key)
        .unwrap_or_else(|| panic!("no {key} in {:?}", keys(s)))
}

#[test]
fn failing_audits_become_findings_with_up_to_three_items() {
    let s = summarize(&report(), "/", "mobile", 90.0, true).unwrap();
    let f = find(&s, "/ mobile unused-javascript");
    assert_eq!(f.title, "Reduce unused JavaScript");
    assert_eq!(f.display_value, "Potential savings of 400 KiB");
    assert_eq!(f.items.len(), 3);
    assert_eq!(find(&s, "/ mobile color-contrast").items, ["div.hero > p"]);
    for passing in ["image-alt", "uses-http2", "manual-check", "first-contentful-paint"] {
        assert!(!keys(&s).iter().any(|k| k.ends_with(passing)), "{passing}");
    }
}

#[test]
fn a_finding_naming_only_other_origins_is_dropped() {
    let s = summarize(&report(), "/", "mobile", 90.0, true).unwrap();
    assert!(!keys(&s).iter().any(|k| k.ends_with("third-party-cookies")));
}

#[test]
fn scores_and_core_web_vitals_are_read() {
    let s = summarize(&report(), "/", "mobile", 90.0, true).unwrap();
    assert_eq!(
        s.scores,
        Scores {
            performance: Some(72),
            accessibility: Some(95),
            best_practices: Some(100),
            seo: Some(63)
        }
    );
    assert_eq!(
        (s.lcp, s.fcp, s.tbt, s.cls),
        (Some(4200.5), Some(1500.0), Some(120.0), Some(0.01))
    );
    assert_eq!(find(&s, "/ mobile score performance").display_value, "72");
}

#[test]
fn a_high_severity_informative_security_audit_is_a_finding() {
    let s = summarize(&report(), "/", "desktop", 90.0, true).unwrap();
    let f = find(&s, "/ desktop csp-xss");
    assert_eq!(f.items, ["No CSP found in enforcement mode"]);
}

#[test]
fn staging_drops_is_crawlable_and_the_seo_score_but_keeps_the_seo_score_in_the_scores() {
    let s = summarize(&report(), "/", "mobile", 90.0, true).unwrap();
    assert!(!keys(&s)
        .iter()
        .any(|k| k.ends_with("is-crawlable") || k.ends_with("score seo")));
    assert_eq!(s.scores.seo, Some(63));
}

#[test]
fn another_failing_seo_audit_stays_a_finding_on_staging() {
    let mut v: serde_json::Value = serde_json::from_str(&report()).unwrap();
    v["audits"]["meta-description"] =
        serde_json::json!({ "title": "No meta description", "score": 0, "scoreDisplayMode": "binary" });
    let s = summarize(&v.to_string(), "/", "mobile", 90.0, true).unwrap();
    assert!(keys(&s).contains(&"/ mobile meta-description"));
}

#[test]
fn without_staging_the_same_report_keeps_is_crawlable_and_the_seo_score() {
    let s = summarize(&report(), "/", "mobile", 90.0, false).unwrap();
    assert!(keys(&s).contains(&"/ mobile is-crawlable"));
    assert!(keys(&s).contains(&"/ mobile score seo"));
}

#[test]
fn a_category_over_min_score_is_no_finding() {
    let s = summarize(&report(), "/about", "mobile", 70.0, true).unwrap();
    assert!(!keys(&s).iter().any(|k| k.contains("score")));
    assert!(keys(&s).contains(&"/about mobile unused-javascript"));
}

#[test]
fn the_lock_name_has_the_days_date() {
    assert_eq!(
        lighthouse_lock(NaiveDate::from_ymd_opt(2026, 10, 6).unwrap()),
        "lighthouse-2026-10-06"
    );
}

fn cfg(enabled: bool, url: Option<&str>) -> ProjectConfig {
    ProjectConfig {
        lighthouse: LighthouseConfig {
            enabled,
            ..LighthouseConfig::default()
        },
        deploy: DeployConfig {
            staging: url.map(|u| DeployTarget {
                branch: "staging".into(),
                url: Some(u.into()),
                smoke: None,
                version_path: None,
            }),
            production: None,
        },
        ..ProjectConfig::default()
    }
}

#[test]
fn the_target_is_none_when_off_a_note_without_a_staging_url_else_the_url_and_pages() {
    assert_eq!(lighthouse_target(&cfg(false, Some("https://s.example.com"))), None);
    assert!(matches!(
        lighthouse_target(&cfg(true, None)),
        Some(LighthouseTarget::Skip { .. })
    ));
    assert!(matches!(
        lighthouse_target(&cfg(true, Some(" "))),
        Some(LighthouseTarget::Skip { .. })
    ));
    assert_eq!(
        lighthouse_target(&cfg(true, Some("https://s.example.com/"))),
        Some(LighthouseTarget::Run {
            staging_url: "https://s.example.com".into(),
            pages: vec!["/".into()]
        })
    );
}
