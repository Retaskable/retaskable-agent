use serde_json::json;

use super::*;

fn opts(staging: bool) -> SummaryOptions<'static> {
    SummaryOptions {
        page: "/",
        preset: Preset::Mobile,
        host: "s.example",
        min_score: 90,
        staging,
    }
}

fn sum(report: serde_json::Value, staging: bool) -> PageSummary {
    summarize(&Report::parse(&report.to_string()), &opts(staging))
}

#[test]
fn scores_round_and_a_category_under_the_minimum_is_a_finding() {
    let s = sum(
        json!({ "categories": {
            "performance": { "title": "Performance", "score": 0.855 },
            "accessibility": { "score": 1 },
            "best-practices": { "score": null },
            "seo": { "title": "SEO", "score": 0.5 }
        } }),
        false,
    );
    assert_eq!(
        s.scores,
        Scores {
            performance: Some(86),
            accessibility: Some(100),
            best_practices: None,
            seo: Some(50)
        }
    );
    let keys: Vec<_> = s.findings.iter().map(|f| (f.key.as_str(), f.title.as_str())).collect();
    assert_eq!(
        keys,
        [
            ("/ mobile score performance", "Performance score 86"),
            ("/ mobile score seo", "SEO score 50")
        ]
    );
    // Staging blocks indexing on purpose: the SEO score is still reported, but isn't a finding.
    let st = sum(json!({ "categories": { "seo": { "score": 0.5 } } }), true);
    assert_eq!(st.scores.seo, Some(50));
    assert!(st.findings.is_empty());
}

#[test]
fn a_failing_audit_names_up_to_three_items_of_its_own_origin() {
    let s = sum(
        json!({ "audits": {
            "unused-js": { "title": "Reduce JS", "score": 0.2, "scoreDisplayMode": "numeric", "displayValue": "1 KiB",
                "details": { "items": [
                    { "url": "https://cdn.other/x.js" },
                    { "url": "https://s.example/a.js" },
                    { "source": { "url": " https://s.example/b.js " } },
                    { "node": { "selector": "div.x" } },
                    { "selector": "p" },
                    { "url": "https://s.example/a.js" }
                ] } },
            "third-party-only": { "title": "T", "score": 0, "details": { "items": [ { "url": "https://cdn.other/y.js" } ] } },
            "fine": { "title": "F", "score": 0.95 },
            "manual-one": { "title": "M", "score": 0, "scoreDisplayMode": "manual" },
            "is-crawlable": { "title": "Crawl", "score": 0 }
        } }),
        true,
    );
    assert_eq!(s.findings.len(), 1, "{:?}", s.findings);
    let f = &s.findings[0];
    assert_eq!((f.key.as_str(), f.title.as_str()), ("/ mobile unused-js", "Reduce JS"));
    assert_eq!(f.display_value.as_deref(), Some("1 KiB"));
    assert_eq!(f.items, ["https://s.example/a.js", "https://s.example/b.js", "div.x"]);
    let loud = sum(
        json!({ "audits": { "is-crawlable": { "title": "Crawl", "score": 0 } } }),
        false,
    );
    assert_eq!(loud.findings[0].key, "/ mobile is-crawlable");
    assert_eq!(loud.findings[0].display_value, None);
}

#[test]
fn findings_keep_the_order_of_the_report() {
    // Written as text: a `json!` object would sort its keys before the report saw them.
    let text = r#"{ "audits": {
        "zzz": { "title": "Z", "score": 0 },
        "aaa": { "title": "A", "score": 0 },
        "mmm": { "title": "M", "score": 0 }
    } }"#;
    let s = summarize(&Report::parse(text), &opts(false));
    let ids: Vec<_> = s.findings.iter().map(|f| f.key.rsplit(' ').next().unwrap()).collect();
    assert_eq!(ids, ["zzz", "aaa", "mmm"]);
}

#[test]
fn a_severe_informative_audit_is_a_finding_and_a_failed_security_check() {
    let s = sum(
        json!({ "audits": {
            "has-hsts": { "title": "HSTS", "score": 1, "scoreDisplayMode": "informative",
                "details": { "items": [ { "severity": "High", "description": " No HSTS header found " },
                                        { "severity": "High", "description": "No HSTS header found" },
                                        { "severity": "Medium", "description": "meh" } ] } },
            "csp-xss": { "title": "CSP", "score": 1, "scoreDisplayMode": "informative", "details": { "items": [] } },
            "is-on-https": { "title": "https", "score": 0, "scoreDisplayMode": "binary" },
            "geolocation-on-start": { "title": "geo", "score": 1, "scoreDisplayMode": "binary" }
        } }),
        false,
    );
    let hsts = s.findings.iter().find(|f| f.key.ends_with("has-hsts")).unwrap();
    assert_eq!(hsts.items, ["No HSTS header found"]);
    assert_eq!(
        s.security,
        BTreeMap::from([
            ("csp-xss".to_string(), true),
            ("geolocation-on-start".to_string(), true),
            ("has-hsts".to_string(), false),
            ("is-on-https".to_string(), false),
        ])
    );
}

#[test]
fn metrics_are_rounded_and_left_out_when_absent() {
    let s = sum(
        json!({ "audits": {
            "largest-contentful-paint": { "numericValue": 1234.5 },
            "first-contentful-paint": { "numericValue": 800.4 },
            "cumulative-layout-shift": { "numericValue": 0.12345 }
        } }),
        false,
    );
    assert_eq!((s.lcp_ms, s.fcp_ms, s.tbt_ms), (Some(1235), Some(800), None));
    assert_eq!(s.cls, Some(0.123));
    let raw = serde_json::to_value(raw_pages(&[s])).unwrap();
    assert_eq!(raw[0]["lcpMs"], 1235);
    assert!(raw[0].get("tbtMs").is_none() && raw[0].get("a11y").is_none() && raw[0].get("security").is_none());
    assert_eq!(raw[0]["scores"]["best-practices"], serde_json::Value::Null);
    assert_eq!(raw[0]["preset"], "mobile");
}

#[test]
fn the_accessibility_audits_that_applied_and_the_manual_count() {
    let s = sum(
        json!({
            "categories": { "accessibility": { "score": 1, "auditRefs": [
                { "id": "color-contrast" }, { "id": "image-alt" }, { "id": "tabindex" },
                { "id": "focus-traps" }, { "id": "not-in-audits" }, { "id": "numeric-one" }, {}
            ] } },
            "audits": {
                "color-contrast": { "title": "Contrast", "score": 0, "scoreDisplayMode": "binary" },
                "image-alt": { "score": 1, "scoreDisplayMode": "binary" },
                "tabindex": { "score": null, "scoreDisplayMode": "notApplicable" },
                "focus-traps": { "score": null, "scoreDisplayMode": "manual" },
                "numeric-one": { "score": 0.5, "scoreDisplayMode": "numeric" }
            }
        }),
        false,
    );
    let a = s.a11y.unwrap();
    assert_eq!(a.manual, 1);
    assert_eq!(
        a.audits,
        [
            A11yAudit {
                id: "color-contrast".into(),
                title: "Contrast".into(),
                pass: false
            },
            A11yAudit {
                id: "image-alt".into(),
                title: "image-alt".into(),
                pass: true
            },
        ]
    );
    assert!(
        sum(json!({ "categories": { "accessibility": { "auditRefs": [] } } }), false)
            .a11y
            .is_none()
    );
}

#[test]
fn text_that_is_not_a_report_reads_as_an_empty_one() {
    let s = summarize(&Report::parse("not json"), &opts(false));
    assert_eq!(s.scores, Scores::default());
    assert!(s.findings.is_empty() && s.security.is_empty());
}

#[test]
fn hosts_carry_a_port_unless_it_is_the_default() {
    assert_eq!(host_of("https://S.example/x"), "s.example");
    assert_eq!(host_of("https://s.example:443/x"), "s.example");
    assert_eq!(host_of("http://s.example:8080/x"), "s.example:8080");
    assert_eq!(host_of("div.x"), "");
}
