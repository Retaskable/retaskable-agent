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

fn draft(status: &str, ago_days: i64) -> Option<DraftState> {
    let now = 100 * 86_400_000;
    Some(DraftState {
        status: status.into(),
        last_entry_ms: now - ago_days * 86_400_000,
    })
}

const NOW: i64 = 100 * 86_400_000;

fn known(d: Option<DraftState>) -> Known {
    Known::from([("/ mobile a".to_string(), d)])
}

#[test]
fn a_key_is_quiet_while_its_draft_is_open_merged_or_recently_discarded() {
    assert!(!is_new_finding("/ mobile a", &known(draft("open", 90)), NOW));
    assert!(!is_new_finding("/ mobile a", &known(draft("merged", 90)), NOW));
    assert!(!is_new_finding("/ mobile a", &known(draft("discarded", 29)), NOW));
    assert!(is_new_finding("/ mobile a", &known(draft("discarded", 31)), NOW));
    assert!(is_new_finding("/ mobile a", &known(draft("failed", 1)), NOW));
    assert!(is_new_finding("/ mobile a", &known(None), NOW));
    assert!(is_new_finding("/ mobile b", &known(draft("open", 1)), NOW));
}

fn summary(keys: &[&str]) -> Summary {
    Summary {
        page: "/".into(),
        preset: "mobile".into(),
        scores: Scores {
            performance: Some(62),
            ..Scores::default()
        },
        findings: keys
            .iter()
            .map(|k| Finding {
                key: format!("/ mobile {k}"),
                title: format!("Title of {k}"),
                display_value: "900 KiB".into(),
                items: vec![format!("https://staging.example.com/{k}.js")],
            })
            .collect(),
        ..Summary::default()
    }
}

#[test]
fn new_findings_keep_each_key_once_and_only_if_new() {
    let s = [summary(&["a", "b"]), summary(&["b", "c"])];
    let k = known(draft("open", 1));
    let keys: Vec<&str> = new_findings(&s, &k, NOW).iter().map(|(_, f)| f.key.as_str()).collect();
    assert_eq!(keys, ["/ mobile b", "/ mobile c"]);
}

#[test]
fn the_request_names_each_finding_with_scores_and_items_and_carries_the_tag_and_keys() {
    let s = [summary(&["a", "b"])];
    let new = new_findings(&s, &Known::new(), NOW);
    let d = lighthouse_request("https://staging.example.com", &s, &new, "- a.md: Alpha")
        .unwrap()
        .unwrap();
    assert!(d.request.starts_with("[Lighthouse]"));
    for want in [
        "/ mobile a",
        "Title of b",
        "900 KiB",
        "https://staging.example.com/b.js",
        "performance 62",
        "- a.md: Alpha",
    ] {
        assert!(d.request.contains(want), "{want}");
    }
    let note = d.intake.unwrap()["note"].as_str().unwrap().to_string();
    assert!(note.contains("/ mobile a") && note.contains("/ mobile b"));
    assert!(lighthouse_request("u", &s, &[], "").unwrap().is_none());
}
