use std::sync::Mutex;

use serde_json::json;

use super::*;
use crate::jobs::a11y_sweep::{CheckSize, Size, SweepOccurrence, SweepResult, Theme, Verdicts};

struct Fake {
    run: Mutex<Option<SweepRun>>,
    seen: Mutex<Vec<(String, Vec<String>, bool)>>,
}

impl Fake {
    fn new(run: SweepRun) -> Fake {
        Fake {
            run: Mutex::new(Some(run)),
            seen: Mutex::new(Vec::new()),
        }
    }
}

impl Sweeper for Fake {
    async fn sweep(&self, base: &str, routes: &[String], prefs: Option<&Value>) -> SweepRun {
        self.seen
            .lock()
            .unwrap()
            .push((base.to_string(), routes.to_vec(), prefs.is_some()));
        self.run.lock().unwrap().take().expect("one sweep")
    }
}

fn settings() -> A11ySettings {
    A11ySettings {
        scan: ScanSettings {
            a11y: true,
            staging_url: Some("https://stage.example/".into()),
            pages: vec!["docs".into()],
            ..Default::default()
        },
        sample_screens: vec!["board".into(), "chat".into()],
    }
}

fn job(args: Value) -> JobSpec {
    JobSpec::from_json(&json!({ "kind": "a11y", "day": "2026-11-17", "args": args })).unwrap()
}

fn occurrence(key: &str, route: &str, elements: &[&str]) -> SweepOccurrence {
    SweepOccurrence {
        route: route.to_string(),
        size: CheckSize::Size(Size::Phone),
        theme: Theme::Dark,
        rule_id: "color-contrast".to_string(),
        elements: elements.iter().map(|s| s.to_string()).collect(),
        key: key.to_string(),
    }
}

fn sweep(failures: Vec<SweepOccurrence>) -> SweepResult {
    SweepResult {
        routes: 2,
        rules: vec![Verdicts {
            id: "color-contrast".into(),
            pass: vec![true, false],
        }],
        failures,
        checks: vec![Verdicts {
            id: "scan-reflow".into(),
            pass: vec![true, true],
        }],
    }
}

#[test]
fn the_target_is_off_a_note_or_the_pages_with_the_sample_on_retaskables_own_site() {
    let mut s = settings();
    s.scan.a11y = false;
    assert_eq!(a11y_target(&s), None);
    s.scan.a11y = true;
    s.scan.staging_url = None;
    assert_eq!(
        a11y_target(&s),
        Some(Err(
            "The accessibility sweep needs a staging url (deploy.staging.url) to run on.".to_string()
        ))
    );
    s.scan.staging_url = Some("https://app.example".into());
    s.scan.production_url = Some("https://app.example".into());
    assert_eq!(
        a11y_target(&s),
        Some(Err(
            "The accessibility sweep runs only on staging, and the staging url is production's.".to_string()
        ))
    );
    let t = a11y_target(&settings()).unwrap().unwrap();
    assert_eq!(
        (t.url.as_str(), t.pages),
        ("https://stage.example", vec!["/docs".to_string()])
    );
    let mut own = settings();
    own.scan.staging_url = Some("https://staging.retaskable.com".into());
    own.scan.pages.clear();
    assert_eq!(
        a11y_target(&own).unwrap().unwrap().pages,
        ["/", "/board?sample", "/chat?sample"]
    );
    assert_eq!(sample_prefs()["flags"]["board"], true);
}

#[tokio::test]
async fn it_refuses_when_off_unrunnable_or_pointed_elsewhere_and_runs_nothing() {
    let fake = Fake::new(SweepRun::Failed {
        no_chrome: false,
        error: "x".into(),
    });
    let chrome = ChromeFlag::default();
    let (j, mut s) = (job(json!({})), settings());
    s.scan.a11y = false;
    let input = A11yInput {
        job: &j,
        settings: &s,
        known: BTreeMap::new(),
        chrome: &chrome,
    };
    assert_eq!(
        a11y_job(&input, &fake).await,
        Outcome::refused("The accessibility sweep isn't on for this project.")
    );
    let (j, s) = (job(json!({ "url": "https://evil.example" })), settings());
    let input = A11yInput {
        job: &j,
        settings: &s,
        known: BTreeMap::new(),
        chrome: &chrome,
    };
    assert_eq!(
        a11y_job(&input, &fake).await,
        Outcome::refused("The sweep won't run: the task's url isn't this project's staging url.")
    );
    assert!(fake.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_good_sweep_answers_the_raw_result_with_deduplicated_cut_failures() {
    let long = "x".repeat(301);
    let failures = vec![
        occurrence("a11y /docs phone dark color-contrast", "/docs", &[]),
        occurrence("a11y /docs phone dark color-contrast", "/docs", &["again"]),
        occurrence(&long, "/docs", &[]),
        occurrence("k2", &long, &[]),
        occurrence("k3", "/docs", &["a", "b", "c", "d", "e", "f", &"y".repeat(200)]),
    ];
    let fake = Fake::new(SweepRun::Ok {
        result: sweep(failures),
        ms: 61_000,
    });
    let (j, s, chrome) = (
        job(json!({ "url": "https://stage.example/other" })),
        settings(),
        ChromeFlag::default(),
    );
    let known = BTreeMap::from([("k".to_string(), "d".to_string())]);
    let input = A11yInput {
        job: &j,
        settings: &s,
        known: known.clone(),
        chrome: &chrome,
    };
    let Outcome::Done { result, known: k } = a11y_job(&input, &fake).await else {
        panic!()
    };
    assert_eq!(k, known);
    assert_eq!(
        *fake.seen.lock().unwrap(),
        [("https://stage.example".to_string(), vec!["/docs".to_string()], false)]
    );
    assert_eq!(
        (result.day.as_str(), result.routes, result.ms),
        ("2026-11-17", 2, 61_000)
    );
    assert_eq!(result.note, None);
    let keys: Vec<&str> = result.failures.iter().map(|f| f.key.as_str()).collect();
    assert_eq!(keys, ["a11y /docs phone dark color-contrast", "k3"]);
    let v = serde_json::to_value(&result).unwrap();
    assert_eq!(
        v["failures"][0],
        json!({ "key": "a11y /docs phone dark color-contrast", "route": "/docs", "size": "phone", "theme": "dark", "ruleId": "color-contrast" })
    );
    assert_eq!(v["failures"][1]["elements"].as_array().unwrap().len(), 5);
    assert_eq!(v["rules"][0], json!({ "id": "color-contrast", "pass": [true, false] }));
    assert_eq!(v["checks"][0]["id"], "scan-reflow");
    assert!(v.get("note").is_none());
}

#[tokio::test]
async fn at_most_120_failures_and_a_slow_sweep_is_noted() {
    let failures: Vec<SweepOccurrence> = (0..150).map(|i| occurrence(&format!("k{i}"), "/", &[])).collect();
    let fake = Fake::new(SweepRun::Ok {
        result: sweep(failures),
        ms: 150_400,
    });
    let (j, s, chrome) = (job(json!({})), settings(), ChromeFlag::default());
    let input = A11yInput {
        job: &j,
        settings: &s,
        known: BTreeMap::new(),
        chrome: &chrome,
    };
    let Outcome::Done { result, .. } = a11y_job(&input, &fake).await else {
        panic!()
    };
    assert_eq!(result.failures.len(), MAX_FAILURES);
    assert_eq!(
        result.note.as_deref(),
        Some("The sweep took 150s, over its 2-minute budget: it needs trimming")
    );
}

#[tokio::test]
async fn the_sample_prefs_go_to_the_sweep_for_sample_routes() {
    let fake = Fake::new(SweepRun::Ok {
        result: sweep(vec![]),
        ms: 1,
    });
    let mut s = settings();
    s.scan.staging_url = Some("https://staging.retaskable.com".into());
    let (j, chrome) = (job(json!({})), ChromeFlag::default());
    let input = A11yInput {
        job: &j,
        settings: &s,
        known: BTreeMap::new(),
        chrome: &chrome,
    };
    a11y_job(&input, &fake).await;
    assert!(fake.seen.lock().unwrap()[0].2);
}

#[tokio::test]
async fn no_chrome_is_a_refusal_and_any_other_failure_is_a_note() {
    let (j, s, chrome) = (job(json!({})), settings(), ChromeFlag::default());
    let input = A11yInput {
        job: &j,
        settings: &s,
        known: BTreeMap::new(),
        chrome: &chrome,
    };
    let fake = Fake::new(SweepRun::Failed {
        no_chrome: true,
        error: "Executable doesn't exist".into(),
    });
    assert_eq!(
        a11y_job(&input, &fake).await,
        Outcome::refused("The accessibility sweep needs Chrome on this computer")
    );
    assert!(chrome.missing());
    let fake = Fake::new(SweepRun::Failed {
        no_chrome: false,
        error: format!("page\ncrashed {}", "z".repeat(400)),
    });
    let Outcome::Failed {
        note,
        no_retry,
        limit_hit,
    } = a11y_job(&input, &fake).await
    else {
        panic!()
    };
    assert!(
        note.starts_with("The accessibility sweep failed: page crashed zzz") && note.chars().count() == 300,
        "{note}"
    );
    assert!(!no_retry && limit_hit.is_none());
}
