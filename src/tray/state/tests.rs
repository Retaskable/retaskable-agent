use super::*;

fn cloud(name: &str, link: LinkState) -> CloudInput {
    CloudInput {
        name: name.into(),
        url: format!("https://{}.example", name.to_lowercase()),
        link_page: None,
        host: format!("{}.example", name.to_lowercase()),
        link,
    }
}

fn input(clouds: Vec<CloudInput>) -> TrayInput {
    TrayInput {
        clouds,
        all_paused: false,
        sessions: 0,
        has_project: true,
        exit: Exit::None,
        backlog: None,
        is_release: false,
        start_at_login: false,
        windows: false,
        machine: "jcnash-pc4".into(),
        tags: vec![],
    }
}

fn waiting() -> LinkState {
    LinkState::Waiting {
        code: Some("WXYZ-2345".into()),
    }
}

#[test]
fn icon_state_for_one_to_four_clouds() {
    use LinkState::*;
    let names = ["Production", "Staging", "Local", "Custom"];
    for n in 1..=4 {
        let all = |l: LinkState| input(names[..n].iter().map(|x| cloud(x, l.clone())).collect());
        assert_eq!(icon_state(&all(NotLinked)), IconState::NotLinked);
        assert_eq!(icon_state(&all(waiting())), IconState::Waiting);
        assert_eq!(icon_state(&all(Offline)), IconState::Offline);
        assert_eq!(icon_state(&all(Connected)), IconState::Connected);
    }
}

#[test]
fn icon_state_worst_first() {
    use LinkState::*;
    let s = |ls: Vec<LinkState>| {
        icon_state(&input(
            ls.into_iter()
                .enumerate()
                .map(|(i, l)| cloud(&format!("C{i}"), l))
                .collect(),
        ))
    };
    assert_eq!(s(vec![Connected, NotLinked]), IconState::Connected);
    assert_eq!(s(vec![Connected, Offline, NotLinked]), IconState::Offline);
    assert_eq!(s(vec![Connected, Offline, waiting(), NotLinked]), IconState::Offline);
    assert_eq!(s(vec![Connected, waiting(), NotLinked]), IconState::Waiting);
    assert_eq!(s(vec![NotLinked, waiting()]), IconState::Waiting);
    assert_eq!(s(vec![]), IconState::NotLinked);
}

#[test]
fn invalid_grant_reads_offline() {
    let i = input(vec![
        cloud("Production", LinkState::UnlinkedOnTheWebsite),
        cloud("Local", LinkState::Connected),
    ]);
    assert_eq!(icon_state(&i), IconState::Offline);
    assert_eq!(
        tooltip(&i),
        "Retaskable · Production: unlinked on the website · Local: connected"
    );
}

#[test]
fn tooltip_forms() {
    let mut i = input(vec![
        cloud("Production", LinkState::Connected),
        cloud("Staging", LinkState::NotLinked),
        cloud("Local", LinkState::Offline),
    ]);
    assert_eq!(tooltip(&i), "Retaskable · Production: connected · Local: offline");
    i.clouds[2].link = waiting();
    assert_eq!(
        tooltip(&i),
        "Retaskable · Production: connected · Local: waiting: WXYZ-2345"
    );
    i.all_paused = true;
    assert_eq!(
        tooltip(&i),
        "Retaskable · Production: connected · Local: waiting: WXYZ-2345 · Paused"
    );
    i.backlog = Some(BacklogWait {
        host: "store.example".into(),
        link_page: None,
        unlinked: false,
    });
    assert_eq!(
        tooltip(&i),
        "Retaskable · Production: connected · Local: waiting: WXYZ-2345 · Paused · 1 waiting for you"
    );
}

#[test]
fn tooltip_with_nothing_linked_names_the_first() {
    let i = input(vec![
        cloud("Staging", LinkState::NotLinked),
        cloud("Production", LinkState::NotLinked),
    ]);
    assert_eq!(tooltip(&i), "Retaskable · Staging: not linked");
    let mut i = input(vec![]);
    i.backlog = Some(BacklogWait {
        host: "store.example".into(),
        link_page: None,
        unlinked: false,
    });
    assert_eq!(tooltip(&i), "Retaskable · 1 waiting for you");
}

#[test]
fn status_line_variants() {
    let mut i = input(vec![]);
    assert_eq!(status_line(&i), "Retaskable · idle");
    i.sessions = 1;
    assert_eq!(status_line(&i), "Retaskable · running (1 session)");
    i.sessions = 2;
    assert_eq!(status_line(&i), "Retaskable · running (2 sessions)");
    i.sessions = 0;
    i.all_paused = true;
    assert_eq!(status_line(&i), "Retaskable · paused");
    i.has_project = false;
    assert_eq!(status_line(&i), "Retaskable · no project set up yet");
    i.sessions = 2;
    assert_eq!(status_line(&i), "Retaskable · no project set up yet");
}

#[test]
fn quitting_and_restarting_come_first() {
    let mut i = input(vec![]);
    i.exit = Exit::QuittingWhenIdle;
    assert_eq!(status_line(&i), "Retaskable · quitting when idle");
    i.sessions = 2;
    assert_eq!(status_line(&i), "Retaskable · quitting when idle (2 sessions)");
    i.exit = Exit::RestartingForNewCode;
    assert_eq!(status_line(&i), "Retaskable · restarting for new code");
    i.has_project = false;
    i.exit = Exit::QuittingWhenIdle;
    assert_eq!(status_line(&i), "Retaskable · quitting when idle (2 sessions)");
}

#[test]
fn restarted_ends_the_line_for_ten_minutes() {
    let mut i = input(vec![]);
    let at = |secs| Exit::Restarted {
        at: "14:05".into(),
        ago: Duration::from_secs(secs),
    };
    i.exit = at(599);
    assert_eq!(status_line(&i), "Retaskable · idle · restarted at 14:05");
    i.sessions = 2;
    assert_eq!(
        status_line(&i),
        "Retaskable · running (2 sessions) · restarted at 14:05"
    );
    i.sessions = 0;
    i.all_paused = true;
    assert_eq!(status_line(&i), "Retaskable · paused · restarted at 14:05");
    i.exit = at(600);
    assert_eq!(status_line(&i), "Retaskable · paused");
}

#[test]
fn computer_line_with_and_without_tags() {
    let mut i = input(vec![]);
    assert_eq!(computer_line(&i), "jcnash-pc4 · tags: none");
    i.tags = vec!["windows".into(), "gpu".into(), "display".into()];
    assert_eq!(computer_line(&i), "jcnash-pc4 · tags: windows, gpu, display");
}

#[test]
fn computer_line_is_cut_to_a_menu_title() {
    let mut i = input(vec![]);
    i.tags = vec!["x".repeat(100)];
    let line = computer_line(&i);
    assert_eq!(line.chars().count(), 70);
    assert!(line.ends_with('…'));
}
