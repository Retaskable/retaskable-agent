use super::*;

fn styles(c: &str) -> FocusStyles {
    FocusStyles {
        outline_color: c.to_string(),
        ..Default::default()
    }
}

fn step(el: Option<i64>, name: &str) -> WalkStep {
    WalkStep {
        element: el,
        role: "button".to_string(),
        name: name.to_string(),
        boxed: Boxed {
            x: 0.0,
            y: 0.0,
            width: 10.0,
            height: 10.0,
        },
        in_viewport: true,
        focused: styles("blue"),
        unfocused: styles("none"),
        hits: vec![Some(Hit {
            on_element: true,
            role: String::new(),
            name: String::new(),
        })],
    }
}

fn steps(els: &[Option<i64>]) -> Vec<WalkStep> {
    els.iter().map(|e| step(*e, &format!("e{}", e.unwrap_or(-1)))).collect()
}

fn cand(el: i64) -> Candidate {
    Candidate {
        element: el,
        role: "link".to_string(),
        name: format!("c{el}"),
    }
}

fn failing(j: &Judged) -> Vec<String> {
    j.failures
        .iter()
        .flat_map(|f| f.elements.clone().unwrap_or_default())
        .collect()
}

#[test]
fn a_whole_walk_names_every_candidate_tab_never_reached() {
    // Round: 1, 2, 4, then back on 1.
    let w = Walk {
        steps: steps(&[Some(1), Some(2), Some(4), Some(1)]),
        candidates: vec![cand(1), cand(2), cand(3), cand(4)],
        escape: None,
    };
    let out = judge_walk(&w);
    assert_eq!((out[0].id, out[0].pass), (KEYBOARD_UNREACHED, false));
    assert_eq!(failing(&out[0]), ["link 'c3'"]);
    assert_eq!(out[0].failures[0].detail, "Tab never reaches it");
    assert!(out[1].pass);
    // Focus leaving for the page is a whole walk as well.
    let w = Walk {
        steps: steps(&[Some(1), None]),
        candidates: vec![cand(1), cand(2)],
        escape: None,
    };
    assert_eq!(failing(&judge_walk(&w)[0]), ["link 'c2'"]);
}

#[test]
fn a_cut_walk_judges_only_up_to_the_last_element_reached() {
    let w = Walk {
        steps: steps(&[Some(1), Some(3)]),
        candidates: vec![cand(1), cand(2), cand(3), cand(4)],
        escape: None,
    };
    assert_eq!(failing(&judge_walk(&w)[0]), ["link 'c2'"]);
    let none = Walk {
        steps: steps(&[Some(9)]),
        candidates: vec![cand(1)],
        escape: None,
    };
    assert!(judge_walk(&none)[0].pass);
}

#[test]
fn an_element_focusable_but_not_drawn_fails() {
    let mut s = steps(&[Some(1), None]);
    s[0].boxed.width = 0.0;
    let out = judge_walk(&Walk {
        steps: s,
        candidates: vec![cand(1)],
        escape: None,
    });
    assert_eq!(out[0].failures[0].detail, "focusable but not drawn (0×0)");
    assert_eq!(failing(&out[0]), ["button 'e1'"]);
}

#[test]
fn a_trap_is_the_same_element_twice_or_ten_presses_on_three_or_fewer() {
    assert_eq!(trapped(&steps(&[Some(1), Some(2), Some(2)])), Some(HashSet::from([2])));
    assert_eq!(trapped(&steps(&[Some(1), Some(2), Some(3)])), None);
    // Circling 2,3,4 after the first (1): ten presses on three elements.
    let mut els = vec![Some(1)];
    for i in 0..10 {
        els.push(Some(2 + i % 3));
    }
    assert_eq!(trapped(&steps(&els)), Some(HashSet::from([2, 3, 4])));
    // The first element in the window is a plain loop, not a trap.
    let looped: Vec<Option<i64>> = (0..12).map(|i| Some(1 + i % 3)).collect();
    assert_eq!(trapped(&steps(&looped)), None);
    // Fewer than ten presses can't be a window.
    assert_eq!(trapped(&steps(&[Some(1), Some(2), Some(3), Some(2), Some(3)])), None);
}

#[test]
fn a_trap_fails_unless_escape_gets_out() {
    let s = steps(&[Some(1), Some(2), Some(2)]);
    let mut w = Walk {
        steps: s,
        candidates: vec![cand(1), cand(2)],
        escape: Some(Some(2)),
    };
    let out = judge_walk(&w);
    assert_eq!((out[1].id, out[1].pass), (KEYBOARD_TRAP, false));
    assert_eq!(
        out[1].failures[0].detail,
        "focus cannot leave these elements with Tab or Escape"
    );
    assert_eq!(failing(&out[1]), ["button 'e2'"]);
    // A trap judges only up to the last element reached: candidate 1 was reached, so nothing is unreached.
    assert!(out[0].pass);
    w.escape = Some(Some(7));
    assert!(judge_walk(&w)[1].pass, "Escape reached an element outside it");
    w.escape = Some(None);
    assert!(judge_walk(&w)[1].pass, "Escape left for the page");
    w.escape = None;
    assert!(!judge_walk(&w)[1].pass, "Escape was not tried");
}

#[test]
fn focus_must_show_and_not_be_covered() {
    let mut s = steps(&[Some(1), Some(2), Some(3), Some(4)]);
    s[0].unfocused = s[0].focused.clone();
    s[1].boxed.height = 0.0;
    s[1].unfocused = s[1].focused.clone();
    let j = judge_focus_styles(&s);
    assert_eq!(
        failing(&j),
        ["button 'e1'"],
        "a 0x0 element is the walk's finding, not this one"
    );
    assert_eq!(j.failures[0].detail, "nothing shows it has focus");

    let mut t = steps(&[Some(1), Some(2), Some(3), Some(4)]);
    let hit = |on: bool, name: &str| {
        Some(Hit {
            on_element: on,
            role: "dialog".to_string(),
            name: name.to_string(),
        })
    };
    t[0].hits = vec![hit(false, "Cookies"), None];
    t[1].hits = vec![None, None];
    t[2].hits = vec![hit(false, "x"), hit(true, "")];
    t[3].in_viewport = false;
    t[3].hits = vec![hit(false, "y")];
    let j = judge_obscured(&t);
    assert_eq!(
        j.failures
            .iter()
            .map(|f| (f.detail.as_str(), f.elements.clone().unwrap()))
            .collect::<Vec<_>>(),
        [
            ("under dialog 'Cookies'", vec!["button 'e1'".to_string()]),
            ("covered", vec!["button 'e2'".to_string()])
        ]
    );
}

#[test]
fn reflow_allows_one_pixel() {
    let r = |sw: f64| ReflowRoute {
        route: "/a".to_string(),
        scroll_width: sw,
        client_width: 320.0,
    };
    assert!(judge_reflow(&[r(321.0)]).pass);
    let j = judge_reflow(&[r(500.0)]);
    assert_eq!(j.failures[0].detail, "needs 500 px at 320");
    assert_eq!(j.failures[0].route.as_deref(), Some("/a"));
}

fn overflow(el: i64, ellipsis: bool) -> Overflow {
    Overflow {
        element: el,
        role: "heading".to_string(),
        name: format!("h{el}"),
        ellipsis,
    }
}

#[test]
fn spacing_fails_on_new_overflow_only_and_names_five() {
    let route = |before: Vec<Overflow>, after: Vec<Overflow>| SpacingRoute {
        route: "/s".to_string(),
        before,
        after,
    };
    assert!(
        judge_spacing(&[route(
            vec![overflow(1, false)],
            vec![overflow(1, false), overflow(2, true)]
        )])
        .pass
    );
    let after: Vec<Overflow> = (1..=8).map(|i| overflow(i, false)).collect();
    let j = judge_spacing(&[route(vec![overflow(1, false)], after)]);
    let f = &j.failures[0];
    assert_eq!(f.detail, "text spills out of its box when spaced");
    assert_eq!((f.elements.as_ref().unwrap().len(), f.more), (5, Some(2)));
    assert_eq!(f.elements.as_ref().unwrap()[0], "heading 'h2'");
}

#[test]
fn titles_must_exist_and_differ() {
    let t = |r: &str, title: &str| RouteTitle {
        route: r.to_string(),
        title: title.to_string(),
    };
    let j = judge_titles(&[
        t("/a", "Home"),
        t("/b", " "),
        t("/c?x=1", "Home"),
        t("/a#top", "Other"),
        t("/d", "Docs"),
    ]);
    assert_eq!(j.failures.len(), 2);
    assert_eq!(
        (j.failures[0].route.as_deref(), j.failures[0].detail.as_str()),
        (Some("/b"), "the page has no title")
    );
    assert_eq!(j.failures[1].routes.as_ref().unwrap(), &["/a", "/c"]);
    assert_eq!(j.failures[1].detail, "2 routes share the title Home");
    assert!(judge_titles(&[t("/a", "A"), t("/b", "B")]).pass);
}

/// A scripted page: each press answers the next element, with the styles it had when it lost focus.
struct Page {
    candidates: Vec<Candidate>,
    presses: Vec<(Option<i64>, Option<FocusStyles>)>,
    after_escape: Option<(Option<i64>, Option<FocusStyles>)>,
    blurred: Option<FocusStyles>,
    pressed: usize,
    escaped: bool,
}

impl Page {
    fn new(presses: &[Option<i64>]) -> Page {
        Page {
            candidates: Vec::new(),
            presses: presses.iter().map(|e| (*e, None)).collect(),
            after_escape: None,
            blurred: None,
            pressed: 0,
            escaped: false,
        }
    }
}

impl WalkPage for Page {
    async fn candidates(&mut self) -> Result<Vec<Candidate>, String> {
        Ok(self.candidates.clone())
    }

    async fn press_tab(&mut self) -> Result<Pressed, String> {
        let (el, prev) = self.presses.get(self.pressed).cloned().ok_or("ran out of presses")?;
        self.pressed += 1;
        Ok(Pressed {
            step: step(el, &format!("e{}", el.unwrap_or(-1))),
            prev,
        })
    }

    async fn press_escape_then_tab(&mut self) -> Result<Pressed, String> {
        self.escaped = true;
        let (el, prev) = self.after_escape.clone().unwrap_or((None, None));
        Ok(Pressed {
            step: step(el, "after"),
            prev,
        })
    }

    async fn blur_and_read(&mut self) -> Result<Option<FocusStyles>, String> {
        Ok(self.blurred.clone())
    }
}

#[tokio::test]
async fn the_walk_stops_on_a_round_and_judges_the_four_checks() {
    let mut p = Page::new(&[Some(1), Some(2), Some(1)]);
    p.candidates = vec![cand(1), cand(2), cand(3)];
    // Element 1 lost focus looking different; 2 lost it looking the same; the last press's styles come from the
    // final blur.
    p.presses[1].1 = Some(styles("none"));
    p.presses[2].1 = Some(styles("blue"));
    p.blurred = Some(styles("blue"));
    let out = walk_checks(&mut p).await.unwrap();
    let ids: Vec<&str> = out.iter().map(|j| j.id).collect();
    assert_eq!(
        ids,
        [KEYBOARD_UNREACHED, KEYBOARD_TRAP, FOCUS_VISIBLE, FOCUS_NOT_HIDDEN]
    );
    assert_eq!(p.pressed, 3);
    assert!(!p.escaped);
    assert_eq!(failing(&out[0]), ["link 'c3'"]);
    assert_eq!(failing(&out[2]), ["button 'e2'"]);
}

#[tokio::test]
async fn a_stuck_walk_tries_escape_once_and_records_where_focus_went() {
    let mut p = Page::new(&[Some(1), Some(2), Some(2)]);
    p.after_escape = Some((Some(1), Some(styles("blue"))));
    p.candidates = vec![cand(1), cand(2)];
    let out = walk_checks(&mut p).await.unwrap();
    assert!(p.escaped);
    assert_eq!(p.pressed, 3);
    assert!(out[1].pass, "Escape reached element 1, outside the trap");
}

#[tokio::test]
async fn a_walk_that_reached_nothing_leaves_the_checks_out_and_errors_pass_up() {
    let mut p = Page::new(&[None]);
    assert!(walk_checks(&mut p).await.unwrap().is_empty());
    let mut broken = Page::new(&[]);
    assert_eq!(walk_checks(&mut broken).await.unwrap_err(), "ran out of presses");
    // 60 presses at most.
    let many: Vec<Option<i64>> = (1..=100).collect::<Vec<i64>>().into_iter().map(Some).collect();
    let mut long = Page::new(&many);
    walk_checks(&mut long).await.unwrap();
    assert_eq!(long.pressed, MAX_PRESSES);
}
