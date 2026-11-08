use super::*;
use crate::tray::state::tests::input;
use crate::tray::state::{BacklogWait, Exit};

fn cloud(name: &str, link: LinkState) -> CloudInput {
    let host = match name {
        "Local" => "localhost:8787".to_string(),
        n => format!("{}.retaskable.com", n.to_lowercase()),
    };
    CloudInput {
        name: name.into(),
        url: format!("https://{host}"),
        link_page: matches!(link, LinkState::Waiting { .. }).then(|| format!("https://{host}/link?code=WXYZ-2345")),
        host,
        link,
    }
}

fn waiting() -> LinkState {
    LinkState::Waiting {
        code: Some("WXYZ-2345".into()),
    }
}

fn three() -> Vec<CloudInput> {
    vec![
        cloud("Production", LinkState::NotLinked),
        cloud("Staging", LinkState::NotLinked),
        cloud("Local", LinkState::NotLinked),
    ]
}

fn get<'a>(m: &'a Menu, id: &str) -> &'a Item {
    m.flatten()
        .into_iter()
        .find(|i| i.id == id)
        .unwrap_or_else(|| panic!("no {id}"))
}

fn ids(m: &Menu) -> Vec<String> {
    m.flatten().iter().map(|i| i.id.clone()).collect()
}

#[test]
fn same_ids_in_every_state() {
    let base = ids(&tray_menu(&input(three())));
    let mut busy = input(vec![cloud("Production", LinkState::Connected)]);
    busy.sessions = 2;
    busy.backlog = Some(BacklogWait {
        host: "s".into(),
        link_page: None,
        unlinked: true,
    });
    let mut release = input(vec![cloud("Staging", waiting())]);
    release.is_release = true;
    let mut four = three();
    four.push(cloud("Custom", LinkState::Offline));
    for i in [busy, release, input(four), input(vec![])] {
        assert_eq!(ids(&tray_menu(&i)), base);
    }
    let mut sorted = base.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), base.len(), "ids are unique");
}

#[test]
fn custom_slot_shows_only_with_a_fourth_cloud() {
    let m = tray_menu(&input(three()));
    assert!(get(&m, "link.3.state").hidden && get(&m, "link.3.do").hidden);
    assert!(!get(&m, "link.2.state").hidden);
    let mut four = three();
    four.push(cloud("Custom", LinkState::NotLinked));
    let m = tray_menu(&input(four));
    assert_eq!(
        get(&m, "link.3.state").text,
        "Custom (custom.retaskable.com): not linked"
    );
    assert!(!get(&m, "link.3.do").hidden);
    let m = tray_menu(&input(vec![cloud("Production", LinkState::NotLinked)]));
    assert!(get(&m, "link.1.state").hidden);
}

#[test]
fn link_items_per_state() {
    let mut c = three();
    c[0].link = LinkState::Connected;
    c[1].link = LinkState::UnlinkedOnTheWebsite;
    c[2] = cloud("Local", waiting());
    let m = tray_menu(&input(c));
    let (p, s, l) = (get(&m, "link.0.do"), get(&m, "link.1.do"), get(&m, "link.2.do"));
    assert_eq!(
        (p.text.as_str(), p.action.clone()),
        (
            "Forget Production",
            Some(Action::Forget("https://production.retaskable.com".into()))
        )
    );
    assert_eq!(s.text, "Link to Staging again");
    assert!(s.enabled);
    assert_eq!(s.action, Some(Action::Link("https://staging.retaskable.com".into())));
    assert_eq!(
        get(&m, "link.2.state").text,
        "Local (localhost:8787): waiting: WXYZ-2345"
    );
    assert_eq!(l.text, "Open the link page to approve WXYZ-2345");
    assert_eq!(
        l.action,
        Some(Action::OpenUrl("https://localhost:8787/link?code=WXYZ-2345".into()))
    );
    assert!(!get(&tray_menu(&input(three())), "link.0.do").text.contains("again"));
    let m = tray_menu(&input(three()));
    assert_eq!(get(&m, "link.2.do").text, "Link to Local");
    assert!(get(&m, "link.2.do").enabled);
    let mut o = three();
    o[0].link = LinkState::Offline;
    assert_eq!(get(&tray_menu(&input(o)), "link.0.do").text, "Forget Production");
}

#[test]
fn open_on_with_none_one_and_several() {
    let m = tray_menu(&input(three()));
    let open = get(&m, "open");
    assert!(!open.enabled && !open.hidden && open.text.contains("link first"));
    assert!(get(&m, "open.many").hidden);

    let mut c = three();
    c[1].link = LinkState::Connected;
    let m = tray_menu(&input(c.clone()));
    assert_eq!(get(&m, "open").text, "Open on staging.retaskable.com");
    assert_eq!(
        get(&m, "open").action,
        Some(Action::OpenUrl("https://staging.retaskable.com/".into()))
    );
    assert!(get(&m, "open.many").hidden);

    c[0].link = LinkState::Connected;
    let m = tray_menu(&input(c));
    assert!(get(&m, "open").hidden);
    let many = get(&m, "open.many");
    assert!(!many.hidden && many.inline);
    assert_eq!(get(&m, "open.0").text, "Production (production.retaskable.com)");
    assert_eq!(get(&m, "open.1").text, "Staging (staging.retaskable.com)");
    assert!(get(&m, "open.2").hidden);
}

#[test]
fn release_build_shows_its_environment_and_its_own_items() {
    let m = tray_menu(&input(three()));
    for id in ["open_app", "start_at_login", "open_log"] {
        assert!(get(&m, id).hidden, "{id}");
    }
    let mut i = input(vec![cloud("Staging", LinkState::NotLinked)]);
    i.is_release = true;
    i.start_at_login = true;
    let m = tray_menu(&i);
    assert!(get(&m, "link.1.state").hidden);
    assert!(!get(&m, "link.0.state").hidden);
    assert!(get(&m, "open").hidden);
    assert_eq!(
        get(&m, "open_app").action,
        Some(Action::Link("https://staging.retaskable.com".into()))
    );
    assert!(get(&m, "start_at_login").checked);
    assert_eq!(get(&m, "start_at_login").text, "Start at login");
    assert_eq!(get(&m, "open_log").action, Some(Action::OpenLog));
    i.windows = true;
    assert_eq!(get(&tray_menu(&i), "start_at_login").text, "Start with Windows");
    i.clouds[0].link = LinkState::Connected;
    assert_eq!(
        get(&tray_menu(&i), "open_app").action,
        Some(Action::OpenUrl("https://staging.retaskable.com/".into()))
    );
    i.has_project = false;
    assert_eq!(
        get(&tray_menu(&i), "open_app").action,
        Some(Action::OpenUrl("https://staging.retaskable.com/start".into()))
    );
    i.clouds[0].link = waiting();
    i.clouds[0].link_page = Some("https://x/link?code=A".into());
    assert_eq!(
        get(&tray_menu(&i), "open_app").action,
        Some(Action::OpenUrl("https://x/link?code=A".into()))
    );
}

#[test]
fn backlog_approval_is_inline_and_hidden_when_nothing_waits() {
    let m = tray_menu(&input(three()));
    assert!(get(&m, "requests").hidden && get(&m, "request.backlog").hidden);
    let mut i = input(three());
    i.backlog = Some(BacklogWait {
        host: "store.example".into(),
        link_page: Some("https://store.example/link?code=A".into()),
        unlinked: false,
    });
    let m = tray_menu(&i);
    assert!(!get(&m, "requests").hidden);
    let b = get(&m, "request.backlog");
    assert!(b.inline && !b.hidden);
    assert_eq!(b.text, "Approve the shared backlog");
    assert_eq!(get(&m, "request.backlog.do").text, "Open the link page");
    assert_eq!(
        get(&m, "request.backlog.do").action,
        Some(Action::OpenUrl("https://store.example/link?code=A".into()))
    );
    i.backlog = Some(BacklogWait {
        host: "store.example".into(),
        link_page: None,
        unlinked: true,
    });
    let m = tray_menu(&i);
    assert_eq!(
        get(&m, "request.backlog").text,
        "Unlinked on store.example: link again?"
    );
    assert_eq!(get(&m, "request.backlog.do").text, "Link again");
    assert_eq!(get(&m, "request.backlog.do").action, Some(Action::LinkBacklog));
}

#[test]
fn quit_items() {
    let m = tray_menu(&input(three()));
    assert_eq!(get(&m, "quit").text, "Quit");
    assert!(get(&m, "quit_when_idle").hidden);
    let mut i = input(three());
    i.sessions = 2;
    let m = tray_menu(&i);
    assert_eq!(get(&m, "quit").text, "Quit now");
    assert_eq!(get(&m, "quit").action, Some(Action::QuitNow));
    let w = get(&m, "quit_when_idle");
    assert!(!w.hidden && w.enabled);
    assert_eq!(w.action, Some(Action::QuitWhenIdle));
    i.exit = Exit::QuittingWhenIdle;
    assert!(!get(&tray_menu(&i), "quit_when_idle").enabled);
}

#[test]
fn pause_and_resume() {
    let m = tray_menu(&input(three()));
    assert_eq!(get(&m, "pause").text, "Pause this computer");
    assert_eq!(get(&m, "pause").action, Some(Action::Pause));
    let mut i = input(three());
    i.all_paused = true;
    let m = tray_menu(&i);
    assert_eq!(get(&m, "pause").text, "Resume this computer");
    assert_eq!(get(&m, "pause").action, Some(Action::Resume));
    i.has_project = false;
    assert!(!get(&tray_menu(&i), "pause").enabled);
    assert_eq!(get(&m, "settings").action, Some(Action::Settings));
}

#[test]
fn no_hideable_item_has_a_submenu() {
    let mut linked = three();
    linked[0].link = LinkState::Connected;
    linked[1].link = LinkState::Connected;
    let mut busy = input(linked);
    busy.sessions = 1;
    busy.is_release = true;
    for i in [input(three()), busy] {
        let m = tray_menu(&i);
        let mut all = m.flatten();
        // Hidden in at least one state: every item that is hidden here, plus the ones always hideable.
        all.retain(|x| !x.children.is_empty());
        for x in all {
            if x.id == "link" {
                continue; // never hidden
            }
            assert!(x.inline, "{} has children but isn't inline", x.id);
        }
    }
    for id in [
        "requests",
        "open",
        "open_app",
        "start_at_login",
        "open_log",
        "quit_when_idle",
        "request.backlog",
        "open.many",
    ] {
        let m = tray_menu(&input(three()));
        let it = get(&m, id);
        if id == "request.backlog" || id == "open.many" {
            assert!(it.inline);
        } else {
            assert!(it.children.is_empty(), "{id}");
        }
    }
}
