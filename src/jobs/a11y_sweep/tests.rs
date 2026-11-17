use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::json;

use super::*;
use crate::jobs::a11y_judge::{Boxed, Candidate, FocusStyles, Pressed, WalkStep};

fn ids(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// Answers each (route, size, theme) pass from a table, and each route's checks.
#[derive(Default)]
struct Table {
    passes: HashMap<(String, Size, Theme), PassResult>,
    checks: HashMap<String, Vec<CheckResult>>,
    cross: Vec<(String, CheckResult)>,
    ran: Mutex<Vec<String>>,
}

impl RouteRunner for Table {
    async fn run_pass(&self, route: &str, pass: &Pass) -> Result<PassResult, String> {
        self.ran
            .lock()
            .unwrap()
            .push(format!("{route} {} {}", pass.size.name(), pass.theme.name()));
        Ok(self
            .passes
            .get(&(route.to_string(), pass.size, pass.theme))
            .cloned()
            .unwrap_or_default())
    }

    async fn run_checks(&self, route: &str) -> Result<Vec<CheckResult>, String> {
        Ok(self.checks.get(route).cloned().unwrap_or_default())
    }

    fn cross_route(&self, _: &[(String, Vec<CheckResult>)]) -> Vec<(String, CheckResult)> {
        self.cross.clone()
    }
}

fn check(id: &str, pass: bool, elements: &[&str]) -> CheckResult {
    CheckResult {
        id: id.to_string(),
        size: CheckSize::Size(Size::Desktop),
        theme: Theme::Light,
        pass,
        elements: ids(elements),
    }
}

fn verdicts(v: &[Verdicts]) -> Vec<(&str, Vec<bool>)> {
    v.iter().map(|x| (x.id.as_str(), x.pass.clone())).collect()
}

#[tokio::test]
async fn a_rule_is_anded_across_the_passes_that_ran_it_and_listed_per_route() {
    let mut t = Table::default();
    let key = |r: &str, s, th| (r.to_string(), s, th);
    t.passes.insert(
        key("/a", Size::Phone, Theme::Light),
        PassResult {
            passed: ids(&["image-alt", "color-contrast"]),
            failed: ids(&["label"]),
        },
    );
    t.passes.insert(
        key("/a", Size::Phone, Theme::Dark),
        PassResult {
            passed: vec![],
            failed: ids(&["color-contrast"]),
        },
    );
    t.passes.insert(
        key("/a", Size::Desktop, Theme::Light),
        PassResult {
            passed: ids(&["color-contrast", "target-size"]),
            failed: vec![],
        },
    );
    // Route two never runs `label` or `target-size`.
    t.passes.insert(
        key("/b", Size::Phone, Theme::Light),
        PassResult {
            passed: ids(&["image-alt", "color-contrast"]),
            failed: vec![],
        },
    );
    let out = sweep_routes(&ids(&["/a", "/b"]), &PASSES, &t).await.unwrap();
    assert_eq!(out.routes, 2);
    assert_eq!(
        verdicts(&out.rules),
        [
            ("image-alt", vec![true, true]),
            ("color-contrast", vec![false, true]),
            ("label", vec![false]),
            ("target-size", vec![true]),
        ]
    );
    let keys: Vec<&str> = out.failures.iter().map(|f| f.key.as_str()).collect();
    assert_eq!(keys, ["a11y /a phone light label", "a11y /a phone dark color-contrast"]);
    assert_eq!(t.ran.lock().unwrap().len(), 8, "every pass on every route");
    assert!(out.checks.is_empty());
}

#[tokio::test]
async fn checks_are_per_route_and_a_cross_route_result_joins_its_route() {
    let mut t = Table::default();
    t.checks.insert(
        "/a".into(),
        vec![
            check("scan-reflow", true, &[]),
            check("scan-focus-visible", false, &["button 'x'"]),
        ],
    );
    t.checks
        .insert("/b".into(), vec![check("scan-reflow", false, &["needs 400 px at 320"])]);
    t.cross = vec![(
        "/a".into(),
        check("scan-page-title", false, &["2 routes share the title Home"]),
    )];
    let out = sweep_routes(&ids(&["/a", "/b"]), &PASSES[..1], &t).await.unwrap();
    assert_eq!(
        verdicts(&out.checks),
        [
            ("scan-reflow", vec![true, false]),
            ("scan-focus-visible", vec![false]),
            ("scan-page-title", vec![false]),
        ]
    );
    let f: Vec<(&str, &str, Vec<String>)> = out
        .failures
        .iter()
        .map(|f| (f.key.as_str(), f.rule_id.as_str(), f.elements.clone()))
        .collect();
    assert_eq!(
        f[0],
        (
            "a11y /a desktop light scan-focus-visible",
            "scan-focus-visible",
            ids(&["button 'x'"])
        )
    );
    assert_eq!(f[1].0, "a11y /a desktop light scan-page-title");
    assert_eq!(f[2].0, "a11y /b desktop light scan-reflow");
}

#[tokio::test]
async fn a_failed_pass_or_checks_run_fails_the_sweep() {
    struct Broken;
    impl RouteRunner for Broken {
        async fn run_pass(&self, _: &str, _: &Pass) -> Result<PassResult, String> {
            Err("net::ERR_CONNECTION_REFUSED".into())
        }
        async fn run_checks(&self, _: &str) -> Result<Vec<CheckResult>, String> {
            Ok(vec![])
        }
        fn cross_route(&self, _: &[(String, Vec<CheckResult>)]) -> Vec<(String, CheckResult)> {
            vec![]
        }
    }
    assert_eq!(
        sweep_routes(&ids(&["/"]), &PASSES, &Broken).await.unwrap_err(),
        "net::ERR_CONNECTION_REFUSED"
    );
}

#[test]
fn the_passes_are_the_phone_light_full_pass_then_only_what_themes_and_sizes_change() {
    assert_eq!(PASSES.len(), 4);
    assert!(matches!(PASSES[0].run_only, RunOnly::Tags(t) if t.contains(&"wcag22aa") && t.len() == 6));
    assert_eq!((PASSES[1].size, PASSES[1].theme), (Size::Phone, Theme::Dark));
    assert!(matches!(PASSES[1].run_only, RunOnly::Rules(["color-contrast"])));
    assert!(matches!(
        PASSES[2].run_only,
        RunOnly::Rules(["color-contrast", "target-size"])
    ));
    assert_eq!(Size::Phone.viewport(), (390, 844));
    assert_eq!(Size::Desktop.viewport(), (1440, 900));
    assert_eq!(CheckSize::Reflow.name(), "320x256");
}

/// A fake browser with per-route answers and a record of what was opened.
#[derive(Default)]
struct Fake {
    titles: HashMap<String, String>,
    reflow: HashMap<String, (f64, f64)>,
    walk_fails: bool,
    spacing_fails: bool,
    walkers: Mutex<Vec<(String, bool)>>,
}

struct OneStop(bool);

impl WalkPage for OneStop {
    async fn candidates(&mut self) -> Result<Vec<Candidate>, String> {
        Ok(vec![])
    }

    async fn press_tab(&mut self) -> Result<Pressed, String> {
        let styles = |c: &str| FocusStyles {
            outline_color: c.to_string(),
            ..Default::default()
        };
        Ok(Pressed {
            step: WalkStep {
                element: self.0.then_some(1),
                role: "button".into(),
                name: "Go".into(),
                boxed: Boxed {
                    x: 0.0,
                    y: 0.0,
                    width: 5.0,
                    height: 5.0,
                },
                in_viewport: true,
                focused: styles("blue"),
                unfocused: styles("blue"),
                hits: vec![],
            },
            prev: None,
        })
    }

    async fn press_escape_then_tab(&mut self) -> Result<Pressed, String> {
        self.press_tab().await
    }

    async fn blur_and_read(&mut self) -> Result<Option<FocusStyles>, String> {
        Ok(None)
    }
}

impl SweepBrowser for Fake {
    type Walker = OneStop;

    async fn run_pass(&self, _: &str, _: &Pass) -> Result<PassResult, String> {
        Ok(PassResult::default())
    }

    async fn walker(&self, route: &str, sample: bool) -> Result<OneStop, String> {
        self.walkers.lock().unwrap().push((route.to_string(), sample));
        if self.walk_fails {
            Err("no page".into())
        } else {
            Ok(OneStop(true))
        }
    }

    async fn reflow(&self, route: &str) -> Result<(f64, f64), String> {
        Ok(self.reflow.get(route).copied().unwrap_or((320.0, 320.0)))
    }

    async fn spacing(&self, _: &str) -> Result<(Vec<Overflow>, Vec<Overflow>), String> {
        if self.spacing_fails {
            Err("evaluate failed".into())
        } else {
            Ok((vec![], vec![]))
        }
    }

    async fn title(&self, route: &str) -> Result<String, String> {
        Ok(self.titles.get(route).cloned().unwrap_or_else(|| route.to_string()))
    }
}

#[tokio::test]
async fn the_browser_runner_judges_a_route_and_its_title_across_routes() {
    let mut b = Fake::default();
    b.titles.insert("/a".into(), "Home".into());
    b.titles.insert("/b?sample".into(), "Home".into());
    b.titles.insert("/c".into(), " ".into());
    b.reflow.insert("/a".into(), (500.0, 320.0));
    let runner = BrowserRunner::new(&b, true);
    let routes = ids(&["/a", "/b?sample", "/c"]);
    let out = sweep_routes(&routes, &PASSES[..1], &runner).await.unwrap();
    // The fake page answers the same element to every press (a trap, with Escape landing on it again, and
    // nothing answering at its points), with the same styles focused and not.
    let checks: Vec<(&str, Vec<bool>)> = verdicts(&out.checks);
    assert_eq!(
        checks,
        [
            ("scan-keyboard-unreached", vec![true, true, true]),
            ("scan-keyboard-trap", vec![false, false, false]),
            ("scan-focus-visible", vec![false, false, false]),
            ("scan-focus-not-hidden", vec![false, false, false]),
            ("scan-reflow", vec![false, true, true]),
            ("scan-text-spacing", vec![true, true, true]),
            ("scan-page-title", vec![false, false, false]),
        ]
    );
    let by_key: HashMap<&str, &SweepOccurrence> = out.failures.iter().map(|f| (f.key.as_str(), f)).collect();
    let reflow = by_key["a11y /a 320x256 light scan-reflow"];
    assert_eq!(reflow.elements, ["needs 500 px at 320"]);
    assert_eq!(
        by_key["a11y /a desktop light scan-page-title"].elements,
        ["2 routes share the title Home"]
    );
    assert_eq!(
        by_key["a11y /c desktop light scan-page-title"].elements,
        ["the page has no title"]
    );
    assert_eq!(
        by_key["a11y /a desktop light scan-focus-visible"].elements,
        ["button 'Go'"]
    );
    // Only a ?sample route is a Sample one, and only when the Sample's prefs are in use.
    assert_eq!(
        *b.walkers.lock().unwrap(),
        [
            ("/a".to_string(), false),
            ("/b?sample".to_string(), true),
            ("/c".to_string(), false)
        ]
    );
}

#[tokio::test]
async fn a_walk_that_fails_leaves_its_checks_out_but_the_other_checks_failing_ends_the_sweep() {
    let b = Fake {
        walk_fails: true,
        ..Default::default()
    };
    let out = sweep_routes(&ids(&["/a"]), &PASSES[..1], &BrowserRunner::new(&b, false))
        .await
        .unwrap();
    let names: Vec<&str> = out.checks.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(names, ["scan-reflow", "scan-text-spacing", "scan-page-title"]);
    let b = Fake {
        spacing_fails: true,
        ..Default::default()
    };
    let err = sweep_routes(&ids(&["/a"]), &PASSES[..1], &BrowserRunner::new(&b, false))
        .await
        .unwrap_err();
    assert_eq!(err, "evaluate failed");
}

struct Launcher(Result<(), LaunchError>);

impl BrowserLauncher for Launcher {
    type Browser = Fake;

    async fn launch(&self, base: &str, prefs: Option<&Value>) -> Result<Fake, LaunchError> {
        assert_eq!(base, "https://s.example");
        assert_eq!(prefs, Some(&json!({ "p": 1 })));
        self.0.clone().map(|()| Fake::default())
    }
}

#[tokio::test]
async fn the_sweeper_launches_sweeps_and_says_why_it_could_not() {
    let prefs = json!({ "p": 1 });
    let routes = ids(&["/a"]);
    let ok = BrowserSweeper(Launcher(Ok(())))
        .sweep("https://s.example", &routes, Some(&prefs))
        .await;
    let SweepRun::Ok { result, .. } = ok else {
        panic!("{ok:?}")
    };
    assert_eq!((result.routes, result.rules.len()), (1, 0));
    let no_chrome = LaunchError {
        no_chrome: true,
        message: "Executable doesn't exist".into(),
    };
    assert_eq!(
        BrowserSweeper(Launcher(Err(no_chrome)))
            .sweep("https://s.example", &routes, Some(&prefs))
            .await,
        SweepRun::Failed {
            no_chrome: true,
            error: "Executable doesn't exist".into()
        }
    );
}
