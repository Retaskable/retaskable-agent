use std::sync::Mutex;

use serde_json::json;

use super::*;
use crate::jobs::lighthouse_reports::PutError;

type Canned = (String, Preset, Result<String, LighthouseOutcome>);

/// Writes the report text a test gives for a url and preset, or fails/misses Chrome.
#[derive(Default)]
struct Fake {
    reports: Mutex<Vec<Canned>>,
    ran: Mutex<Vec<String>>,
}

impl Fake {
    fn canned(&self, url: &str, preset: Preset, r: Result<String, LighthouseOutcome>) {
        self.reports.lock().unwrap().push((url.to_string(), preset, r));
    }
}

impl LighthouseRunner for Fake {
    async fn run(&self, url: &str, preset: Preset, output: &Path) -> LighthouseOutcome {
        self.ran.lock().unwrap().push(format!("{url} {}", preset.name()));
        let found = self
            .reports
            .lock()
            .unwrap()
            .iter()
            .find(|(u, p, _)| u == url && *p == preset)
            .map(|(_, _, r)| r.clone());
        match found {
            Some(Ok(text)) => {
                std::fs::create_dir_all(output.parent().unwrap()).unwrap();
                std::fs::write(output, text).unwrap();
                LighthouseOutcome::Ok(output.to_path_buf())
            }
            Some(Err(o)) => o,
            None => LighthouseOutcome::Failed("no run".to_string()),
        }
    }
}

struct NoStore;
impl ReportStore for NoStore {
    async fn put(&self, _: &str, _: &str, _: Vec<u8>) -> Result<String, PutError> {
        Err(PutError {
            status: None,
            message: "none".into(),
        })
    }
}

fn settings() -> LighthouseSettings {
    LighthouseSettings {
        enabled: true,
        staging_url: Some(" https://stage.example/ ".to_string()),
        pages: vec!["".into(), "docs".into(), " /a ".into()],
        min_score: 90,
    }
}

fn job(args: serde_json::Value) -> JobSpec {
    JobSpec::from_json(&json!({ "kind": "lighthouse", "day": "2026-11-17", "args": args })).unwrap()
}

fn good(perf: f64) -> String {
    json!({
        "categories": { "performance": { "title": "Performance", "score": perf } },
        "audits": { "unused-js": { "title": "JS", "score": 0, "details": { "items": [ { "url": "https://stage.example/a.js" } ] } } }
    })
    .to_string()
}

#[test]
fn the_target_is_off_a_note_or_the_pages() {
    let mut s = settings();
    s.enabled = false;
    assert_eq!(target(&s), None);
    let mut s = settings();
    s.staging_url = Some("  ".into());
    assert_eq!(
        target(&s),
        Some(Err(
            "Lighthouse needs a staging url (deploy.staging.url) to run on.".to_string()
        ))
    );
    s.staging_url = Some("not a url".into());
    assert_eq!(
        target(&s),
        Some(Err(
            "Lighthouse can't run on the staging url not a url: it isn't a url.".to_string()
        ))
    );
    let t = target(&settings()).unwrap().unwrap();
    assert_eq!(t.url, "https://stage.example");
    assert_eq!(t.host, "stage.example");
    assert_eq!(t.pages, ["/docs", "/a"]);
    let mut s = settings();
    s.pages = vec![" ".into()];
    assert_eq!(target(&s).unwrap().unwrap().pages, ["/"]);
}

#[tokio::test]
async fn it_refuses_when_off_unconfigured_or_pointed_elsewhere() {
    let tmp = tempfile::tempdir().unwrap();
    let chrome = ChromeFlag::default();
    let fake = Fake::default();
    let go = |s: LighthouseSettings, j: JobSpec| {
        let (chrome, fake) = (&chrome, &fake);
        let dir = tmp.path().to_path_buf();
        async move {
            let input = LighthouseInput {
                job: &j,
                settings: &s,
                project_dir: &dir,
                known: BTreeMap::new(),
                chrome,
            };
            lighthouse_job(&input, fake, None::<&NoStore>, &ReportFlush::default()).await
        }
    };
    let mut off = settings();
    off.enabled = false;
    assert_eq!(
        go(off, job(json!({}))).await,
        Outcome::refused("Lighthouse isn't on for this project.")
    );
    let mut bare = settings();
    bare.staging_url = None;
    assert!(matches!(go(bare, job(json!({}))).await, Outcome::Refused(n) if n.contains("needs a staging url")));
    assert_eq!(
        go(settings(), job(json!({ "url": "https://evil.example" }))).await,
        Outcome::refused("Lighthouse won't run: the task's url isn't this project's staging url.")
    );
    assert!(fake.ran.lock().unwrap().is_empty(), "nothing ran");
}

#[tokio::test]
async fn it_runs_each_page_on_both_presets_and_answers_the_days_raw_result() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = Fake::default();
    for page in ["/docs", "/a"] {
        let url = format!("https://stage.example{page}");
        fake.canned(&url, Preset::Mobile, Ok(good(0.8)));
        fake.canned(&url, Preset::Desktop, Ok(good(0.95)));
    }
    let (s, j, chrome) = (
        settings(),
        job(json!({ "url": "https://stage.example/other/path" })),
        ChromeFlag::default(),
    );
    let known = BTreeMap::from([("k".to_string(), "draft-1".to_string())]);
    let input = LighthouseInput {
        job: &j,
        settings: &s,
        project_dir: tmp.path(),
        known: known.clone(),
        chrome: &chrome,
    };
    let out = lighthouse_job(&input, &fake, None::<&NoStore>, &ReportFlush::default()).await;
    let Outcome::Done { result, known: k } = out else {
        panic!("{out:?}")
    };
    assert_eq!(k, known);
    assert_eq!(
        *fake.ran.lock().unwrap(),
        [
            "https://stage.example/docs mobile",
            "https://stage.example/docs desktop",
            "https://stage.example/a mobile",
            "https://stage.example/a desktop"
        ]
    );
    assert_eq!(
        (result.day.as_str(), result.first_page.as_str()),
        ("2026-11-17", "/docs")
    );
    assert_eq!(result.pages.len(), 4);
    assert_eq!(result.note, None);
    // The perf score under 90 and the failing audit, once per page and preset; the same key never twice.
    let keys: Vec<_> = result.findings.iter().map(|f| f.key.as_str()).collect();
    assert_eq!(
        keys,
        [
            "/docs mobile score performance",
            "/docs mobile unused-js",
            "/docs desktop unused-js",
            "/a mobile score performance",
            "/a mobile unused-js",
            "/a desktop unused-js"
        ]
    );
    let v = serde_json::to_value(&result).unwrap();
    assert_eq!(v["pages"][0]["scores"]["performance"], 80);
    assert_eq!(v["findings"][1]["items"], json!(["https://stage.example/a.js"]));
    assert!(v.get("note").is_none());
    // The reports wait on disk for the store (none given here).
    assert!(tmp.path().join("lighthouse/2026-11-17/docs-mobile.json").is_file());
}

#[tokio::test]
async fn some_failed_runs_are_a_note_and_all_failed_is_a_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = Fake::default();
    let mut s = settings();
    s.pages = vec!["/".into()];
    fake.canned("https://stage.example/", Preset::Mobile, Ok(good(1.0)));
    fake.canned(
        "https://stage.example/",
        Preset::Desktop,
        Err(LighthouseOutcome::Failed("boom\nbang".into())),
    );
    let (j, chrome) = (job(json!({})), ChromeFlag::default());
    let input = LighthouseInput {
        job: &j,
        settings: &s,
        project_dir: tmp.path(),
        known: BTreeMap::new(),
        chrome: &chrome,
    };
    let Outcome::Done { result, .. } = lighthouse_job(&input, &fake, None::<&NoStore>, &ReportFlush::default()).await
    else {
        panic!()
    };
    assert_eq!(result.note.as_deref(), Some("1 of 2 runs failed: boom bang"));
    assert_eq!(result.pages.len(), 1);

    let all = Fake::default();
    all.canned("https://stage.example/", Preset::Mobile, Ok("not json".into()));
    let out = lighthouse_job(&input, &all, None::<&NoStore>, &ReportFlush::default()).await;
    let Outcome::Failed { note, no_retry, .. } = out else {
        panic!()
    };
    assert!(!no_retry);
    assert!(
        note.starts_with(
            "Lighthouse failed on every page: Lighthouse's report on https://stage.example/ (mobile) couldn't be read"
        ),
        "{note}"
    );
}

#[tokio::test]
async fn no_chrome_is_a_refusal_that_turns_chrome_off_in_can() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = Fake::default();
    fake.canned(
        "https://stage.example/docs",
        Preset::Mobile,
        Err(LighthouseOutcome::ChromeMissing),
    );
    let (s, j, chrome) = (settings(), job(json!({})), ChromeFlag::default());
    let input = LighthouseInput {
        job: &j,
        settings: &s,
        project_dir: tmp.path(),
        known: BTreeMap::new(),
        chrome: &chrome,
    };
    let out = lighthouse_job(&input, &fake, None::<&NoStore>, &ReportFlush::default()).await;
    assert_eq!(out, Outcome::refused("Lighthouse needs Chrome on this computer"));
    assert!(chrome.missing());
    assert_eq!(fake.ran.lock().unwrap().len(), 1, "stopped at the first");
}

#[tokio::test]
async fn old_report_days_are_pruned_and_uploaded_ones_leave_the_disk() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("lighthouse/2026-10-01")).unwrap();
    struct Holds;
    impl ReportStore for Holds {
        async fn put(&self, _: &str, _: &str, bytes: Vec<u8>) -> Result<String, PutError> {
            Ok(format!("{:x}", sha2::Sha256::digest(&bytes)))
        }
    }
    use sha2::Digest;
    let fake = Fake::default();
    let mut s = settings();
    s.pages = vec!["/".into()];
    for p in Preset::ALL {
        fake.canned("https://stage.example/", p, Ok(good(1.0)));
    }
    let (j, chrome) = (job(json!({})), ChromeFlag::default());
    let input = LighthouseInput {
        job: &j,
        settings: &s,
        project_dir: tmp.path(),
        known: BTreeMap::new(),
        chrome: &chrome,
    };
    let out = lighthouse_job(&input, &fake, Some(&Holds), &ReportFlush::default()).await;
    assert!(matches!(out, Outcome::Done { .. }));
    assert!(!tmp.path().join("lighthouse/2026-10-01").exists(), "older than 14 days");
    assert!(
        !tmp.path().join("lighthouse/2026-11-17").exists(),
        "uploaded, so removed"
    );
}

#[test]
fn findings_are_deduplicated_capped_and_cut() {
    let f = |key: &str, title: &str| crate::jobs::lighthouse_summary::Finding {
        key: key.to_string(),
        page: "/".into(),
        preset: Preset::Mobile,
        title: title.to_string(),
        display_value: Some("d".repeat(150)),
        items: vec!["i".repeat(250); 5],
    };
    let mut findings = vec![f("a", &"t".repeat(300)), f("a", "dup"), f(&"k".repeat(401), "long")];
    findings.extend((0..50).map(|n| f(&format!("k{n}"), "x")));
    let page = PageSummary {
        page: "/".into(),
        preset: Preset::Mobile,
        scores: Default::default(),
        lcp_ms: None,
        fcp_ms: None,
        tbt_ms: None,
        cls: None,
        a11y: None,
        security: BTreeMap::new(),
        findings,
    };
    let out = result_findings(&[page]);
    assert_eq!(out.len(), MAX_FINDINGS);
    assert_eq!(out[0].title.len(), 200);
    assert_eq!(out[0].display_value.as_deref().unwrap().len(), 100);
    assert_eq!((out[0].items.len(), out[0].items[0].len()), (3, 200));
    assert_eq!(out[1].key, "k0");
}
