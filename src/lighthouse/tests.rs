use super::*;
use crate::config::LighthouseConfig;
use serde_json::json;
use std::path::PathBuf;

fn report() -> Value {
    let path: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "src",
        "lighthouse",
        "fixtures",
        "report.json",
    ]
    .iter()
    .collect();
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn find<'a>(s: &'a Summary, key: &str) -> Option<&'a Finding> {
    s.findings.iter().find(|f| f.key == key)
}

#[test]
fn failing_audits_become_findings_with_up_to_three_items() {
    let s = summarize(&report(), "/", "mobile", 90, false);
    let f = find(&s, "/ mobile unused-javascript").unwrap();
    assert_eq!(f.title, "Reduce unused JavaScript");
    assert_eq!(f.display_value, "Potential savings of 900 KiB");
    assert_eq!(f.items.len(), 3);
    assert_eq!(f.items[0], "https://staging.example.com/_expo/static/js/web/entry.js");
    let c = find(&s, "/ mobile color-contrast").unwrap();
    assert_eq!(c.items, vec!["div.hero > p"]);
    assert!(find(&s, "/ mobile largest-contentful-paint").is_some());
    // passing, informative, manual and not-applicable audits are not findings
    for id in [
        "errors-in-console",
        "uses-http2",
        "is-on-https",
        "viewport",
        "first-contentful-paint",
    ] {
        assert!(find(&s, &format!("/ mobile {id}")).is_none(), "{id}");
    }
}

#[test]
fn findings_naming_only_other_origins_are_dropped() {
    let s = summarize(&report(), "/", "mobile", 90, false);
    assert!(find(&s, "/ mobile third-party-summary").is_none());
}

#[test]
fn scores_and_core_web_vitals_are_read() {
    let s = summarize(&report(), "/", "mobile", 90, false);
    assert_eq!(
        s.scores,
        Scores {
            performance: Some(62),
            accessibility: Some(95),
            best_practices: Some(93),
            seo: Some(63)
        }
    );
    assert_eq!(
        (s.lcp, s.fcp, s.tbt, s.cls),
        (Some(4200.5), Some(1100.0), Some(150.0), Some(0.02))
    );
    assert_eq!(find(&s, "/ mobile score performance").unwrap().display_value, "62");
    assert!(find(&s, "/ mobile score accessibility").is_none());
    let lenient = summarize(&report(), "/", "mobile", 60, false);
    assert!(find(&lenient, "/ mobile score performance").is_none());
}

#[test]
fn a_high_severity_informative_security_audit_is_a_finding() {
    let s = summarize(&report(), "/", "desktop", 90, false);
    let f = find(&s, "/ desktop csp-xss").unwrap();
    assert_eq!(f.items, vec!["No CSP found in enforcement mode"]);
    assert!(find(&s, "/ desktop has-hsts").is_none());
}

#[test]
fn staging_drops_is_crawlable_and_the_seo_score_but_keeps_the_score() {
    let s = summarize(&report(), "/", "mobile", 90, true);
    assert!(find(&s, "/ mobile is-crawlable").is_none());
    assert!(find(&s, "/ mobile score seo").is_none());
    assert_eq!(s.scores.seo, Some(63));
    assert!(find(&s, "/ mobile score performance").is_some());
}

#[test]
fn another_failing_seo_audit_stays_a_finding_on_staging() {
    let mut r = report();
    r["audits"]["meta-description"] = json!({
        "id": "meta-description", "title": "Document has a meta description", "score": 0, "scoreDisplayMode": "binary"
    });
    let s = summarize(&r, "/", "mobile", 90, true);
    assert!(find(&s, "/ mobile meta-description").is_some());
    assert!(find(&s, "/ mobile is-crawlable").is_none());
}

#[test]
fn without_staging_the_same_report_keeps_the_seo_findings() {
    let s = summarize(&report(), "/", "mobile", 90, false);
    assert!(find(&s, "/ mobile is-crawlable").is_some());
    assert!(find(&s, "/ mobile score seo").is_some());
}

#[test]
fn keys_and_the_lock_have_the_specs_form() {
    let s = summarize(&report(), "/pricing", "desktop", 90, false);
    assert!(find(&s, "/pricing desktop color-contrast").is_some());
    assert!(find(&s, "/pricing desktop score performance").is_some());
    assert_eq!(lighthouse_lock("2026-11-08"), "lighthouse-2026-11-08");
}

#[test]
fn the_target_is_none_when_off_and_a_note_without_a_staging_url() {
    let mut cfg = ProjectConfig::default();
    assert_eq!(lighthouse_target(&cfg), None);
    cfg.lighthouse = LighthouseConfig {
        enabled: true,
        ..LighthouseConfig::default()
    };
    assert!(matches!(lighthouse_target(&cfg), Some(LighthouseTarget::Note(n)) if n.contains("staging")));
    cfg.deploy.staging = Some(crate::config::DeployTarget {
        rollout: None,
        branch: "staging".into(),
        url: Some("https://staging.example.com".into()),
        smoke: None,
        version_path: None,
    });
    assert_eq!(
        lighthouse_target(&cfg),
        Some(LighthouseTarget::Run {
            url: "https://staging.example.com".into(),
            pages: vec!["/".into()]
        })
    );
}

const NOW: i64 = 100 * DAY_MS;

fn known(status: &str, days_ago: i64) -> Known {
    let d = DraftState {
        status: status.into(),
        last_entry_ms: NOW - days_ago * DAY_MS,
    };
    Known::from([("k".to_string(), Some(d))])
}

#[test]
fn a_key_is_quiet_while_its_draft_is_open_or_merged() {
    assert!(is_new_finding("other", &known("open", 1), NOW));
    assert!(!is_new_finding("k", &known("open", 90), NOW));
    assert!(!is_new_finding("k", &known("merged", 90), NOW));
}

#[test]
fn a_discarded_draft_is_quiet_for_30_days() {
    assert!(!is_new_finding("k", &known("discarded", 29), NOW));
    assert!(is_new_finding("k", &known("discarded", 31), NOW));
}

#[test]
fn a_failed_or_missing_draft_makes_the_finding_new() {
    assert!(is_new_finding("k", &known("failed", 1), NOW));
    assert!(is_new_finding("k", &Known::from([("k".to_string(), None)]), NOW));
}

fn two_summaries() -> Vec<Summary> {
    let s = summarize(&report(), "/", "mobile", 90, false);
    vec![s.clone(), s]
}

#[test]
fn duplicates_are_kept_once_and_known_keys_dropped() {
    let sums = two_summaries();
    let all = new_findings(&sums, &Known::new(), NOW);
    assert_eq!(all.len(), sums[0].findings.len());
    let key = sums[0].findings[0].key.clone();
    let mut k = known("open", 1);
    k.insert(key.clone(), k["k"].clone());
    assert!(new_findings(&sums, &k, NOW).iter().all(|(_, f)| f.key != key));
}

#[test]
fn the_request_names_each_finding_with_scores_and_items_and_is_tagged() {
    let sums = two_summaries();
    let new = new_findings(&sums, &Known::new(), NOW);
    let d = lighthouse_request(&sums, &new, "- docs/specs/a.md").unwrap();
    assert!(d.request.starts_with("[Lighthouse]"));
    assert!(is_lighthouse_request(&d.request));
    assert!(d.request.contains("docs/specs/a.md"));
    assert!(d.request.contains("accessibility"));
    for (_, f) in &new {
        assert!(d.request.contains(&f.key), "{}", f.key);
        assert!(f.items.iter().all(|i| d.request.contains(i.as_str())));
    }
    let note = d.intake.unwrap()["note"].as_str().unwrap().to_string();
    assert!(note.contains(&new[0].1.key));
}
