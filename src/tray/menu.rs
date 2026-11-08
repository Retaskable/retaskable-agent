//! The tray's menu as plain data (tray/04-the-tray-agent.md sections 4.2 and 4.6), following `trayMenu`: the same
//! items in every state, each with a stable id, so the helper (which builds its menu once) is only told what
//! changed. Only text, enabled and hidden change. An item that can be hidden never has a submenu: it is `inline`,
//! and its items are drawn as indented lines under it.
#[cfg(test)]
mod tests;

use super::state::{CloudInput, LinkState, TrayInput, computer_line, link_text, status_line};
use crate::text::clip;

/// Link to's slots: Production, Staging, Local and a Custom `cloud.url`.
const MAX_LINKS: usize = 4;
const TITLE_MAX: usize = 70;
pub const SEPARATOR: &str = "<SEPARATOR>";

/// What a click runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// The device flow against that cloud's URL, opening its link page.
    Link(String),
    /// Delete that cloud's token and close its connection.
    Forget(String),
    /// Link the shared backlog's store again.
    LinkBacklog,
    OpenUrl(String),
    Pause,
    Resume,
    Settings,
    ToggleStartAtLogin,
    OpenLog,
    QuitNow,
    QuitWhenIdle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// Stable across states.
    pub id: String,
    pub text: String,
    pub enabled: bool,
    pub hidden: bool,
    /// A tick beside it.
    pub checked: bool,
    pub inline: bool,
    pub children: Vec<Item>,
    pub action: Option<Action>,
}

impl Item {
    fn new(id: impl Into<String>, text: impl Into<String>) -> Self {
        Item {
            id: id.into(),
            text: text.into(),
            enabled: true,
            hidden: false,
            checked: false,
            inline: false,
            children: vec![],
            action: None,
        }
    }
    fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }
    fn hidden_if(mut self, hidden: bool) -> Self {
        self.hidden = hidden;
        self
    }
    fn enabled_if(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
    fn does(mut self, action: Action) -> Self {
        self.action = Some(action);
        self
    }
    fn with(mut self, children: Vec<Item>) -> Self {
        self.children = children;
        self
    }
    fn inline(mut self) -> Self {
        self.inline = true;
        self
    }
    /// An empty, greyed-out, hidden placeholder.
    fn empty(id: impl Into<String>) -> Self {
        Item::new(id, "").disabled().hidden_if(true)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Menu {
    pub items: Vec<Item>,
}

impl Menu {
    /// Every item, depth first.
    pub fn flatten(&self) -> Vec<&Item> {
        fn walk<'a>(items: &'a [Item], out: &mut Vec<&'a Item>) {
            for i in items {
                out.push(i);
                walk(&i.children, out);
            }
        }
        let mut out = vec![];
        walk(&self.items, &mut out);
        out
    }
}

/// Linked: a token is held, whether or not its connection is open.
fn is_linked(c: &CloudInput) -> bool {
    matches!(c.link, LinkState::Connected | LinkState::Offline)
}

fn sep(id: &str) -> Item {
    Item::new(id, SEPARATOR)
}

pub fn tray_menu(input: &TrayInput) -> Menu {
    let own = input.is_release.then(|| input.clouds.first()).flatten();
    let linked: Vec<&CloudInput> = input.clouds.iter().filter(|c| is_linked(c)).collect();
    let one = (linked.len() == 1).then(|| linked[0]);
    let busy = input.sessions > 0;
    let all_paused = input.has_project && input.all_paused;
    let mut items = vec![
        Item::new("status", status_line(input)).disabled(),
        Item::new("machine", computer_line(input)).disabled(),
        Item::new("requests", "Waiting for you")
            .disabled()
            .hidden_if(input.backlog.is_none()),
        backlog_item(input),
        sep("sep.requests"),
    ];

    let mut open_app = Item::new("open_app", "Open Retaskable")
        .enabled_if(own.is_some())
        .hidden_if(own.is_none());
    if let Some(c) = own {
        open_app = open_app.does(open_app_action(c, input.has_project));
    }
    items.push(open_app);

    let slots: Vec<Item> = (0..MAX_LINKS)
        .flat_map(|i| link_items(input.clouds.get(i), i))
        .collect();
    items.push(Item::new("link", "Link to").with(slots));

    // Both the plain entry and the many-entry list are there; one is hidden.
    let mut open = Item::new(
        "open",
        match one {
            Some(c) => format!("Open on {}", c.host),
            None => "Open on retaskable.com (link first)".into(),
        },
    )
    .enabled_if(one.is_some() && own.is_none())
    .hidden_if(own.is_some() || linked.len() > 1);
    if let Some(c) = one {
        open = open.does(Action::OpenUrl(format!("{}/", c.url)));
    }
    items.push(open);
    let many = (0..MAX_LINKS)
        .map(|i| match linked.get(i).filter(|_| linked.len() > 1) {
            Some(c) => Item::new(format!("open.{i}"), format!("{} ({})", c.name, c.host))
                .does(Action::OpenUrl(format!("{}/", c.url))),
            None => Item::empty(format!("open.{i}")),
        })
        .collect();
    items.push(
        Item::new("open.many", "Open on retaskable.com")
            .enabled_if(linked.len() > 1)
            .hidden_if(linked.len() < 2)
            .inline()
            .with(many),
    );
    items.push(sep("sep.link"));

    items.push(
        Item::new(
            "pause",
            if all_paused {
                "Resume this computer"
            } else {
                "Pause this computer"
            },
        )
        .enabled_if(input.has_project)
        .does(if all_paused { Action::Resume } else { Action::Pause }),
    );
    items.push(Item::new("settings", "Settings…").does(Action::Settings));
    let mut login = Item::new(
        "start_at_login",
        if input.windows {
            "Start with Windows"
        } else {
            "Start at login"
        },
    )
    .enabled_if(input.is_release)
    .hidden_if(!input.is_release)
    .does(Action::ToggleStartAtLogin);
    login.checked = input.is_release && input.start_at_login;
    items.push(login);
    items.push(
        Item::new("open_log", "Open the log")
            .enabled_if(input.is_release)
            .hidden_if(!input.is_release)
            .does(Action::OpenLog),
    );
    items.push(
        Item::new("quit_when_idle", "Quit when idle")
            .enabled_if(busy && input.exit != super::state::Exit::QuittingWhenIdle)
            .hidden_if(!busy)
            .does(Action::QuitWhenIdle),
    );
    items.push(Item::new("quit", if busy { "Quit now" } else { "Quit" }).does(Action::QuitNow));
    Menu { items }
}

/// Open Retaskable's target: while a code waits its approve page; not linked (or unlinked on the website) a new
/// code; linked, `/start` while this computer has no project, `/` after.
fn open_app_action(c: &CloudInput, has_project: bool) -> Action {
    match (&c.link, &c.link_page) {
        (LinkState::Waiting { .. }, Some(page)) => Action::OpenUrl(page.clone()),
        _ if !is_linked(c) => Action::Link(c.url.clone()),
        _ => Action::OpenUrl(format!("{}/{}", c.url, if has_project { "" } else { "start" })),
    }
}

/// A cloud's two rows in Link to: its state, then what can be done. An empty slot (no Custom cloud) is hidden.
fn link_items(c: Option<&CloudInput>, i: usize) -> Vec<Item> {
    let (state_id, do_id) = (format!("link.{i}.state"), format!("link.{i}.do"));
    let Some(c) = c else {
        return vec![Item::empty(state_id), Item::empty(do_id)];
    };
    let state = clip(&format!("{} ({}): {}", c.name, c.host, link_text(&c.link)), TITLE_MAX);
    let act = match (&c.link, &c.link_page) {
        (LinkState::Waiting { code: Some(code) }, Some(page)) => {
            Item::new(do_id, format!("Open the link page to approve {code}")).does(Action::OpenUrl(page.clone()))
        }
        _ if is_linked(c) => Item::new(do_id, format!("Forget {}", c.name)).does(Action::Forget(c.url.clone())),
        _ => {
            let again = c.link == LinkState::UnlinkedOnTheWebsite;
            Item::new(
                do_id,
                format!("Link to {}{}", c.name, if again { " again" } else { "" }),
            )
            .enabled_if(again || c.link == LinkState::NotLinked)
            .does(Action::Link(c.url.clone()))
        }
    };
    vec![Item::new(state_id, state).disabled(), act]
}

/// The shared backlog's approval under Waiting for you; always there, hidden while nothing waits.
fn backlog_item(input: &TrayInput) -> Item {
    let Some(b) = &input.backlog else {
        return Item::new("request.backlog", "")
            .disabled()
            .hidden_if(true)
            .inline()
            .with(vec![
                Item::empty("request.backlog.who"),
                Item::empty("request.backlog.do"),
            ]);
    };
    let (title, line, act) = if b.unlinked {
        (
            format!("Unlinked on {}: link again?", b.host),
            "Hosted projects get no work until it's linked",
            Item::new("request.backlog.do", "Link again").does(Action::LinkBacklog),
        )
    } else {
        let mut open = Item::new("request.backlog.do", "Open the link page").enabled_if(b.link_page.is_some());
        if let Some(page) = &b.link_page {
            open = open.does(Action::OpenUrl(page.clone()));
        }
        (
            "Approve the shared backlog".to_string(),
            "Hosted projects get no work until it's approved",
            open,
        )
    };
    Item::new("request.backlog", clip(&title, TITLE_MAX))
        .inline()
        .with(vec![Item::new("request.backlog.who", line).disabled(), act])
}
