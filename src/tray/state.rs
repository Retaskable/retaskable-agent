//! What the tray shows, as plain functions over what the agent knows (tray/04-the-tray-agent.md sections 4.1 and
//! 4.6): the icon's state, the tooltip, the status line and the greyed-out computer line. No I/O.
#[cfg(test)]
pub(crate) mod tests;

use std::time::Duration;

use crate::text::clip;

/// Menu titles are one line: longer ones are cut.
const TITLE_MAX: usize = 70;

/// How long "restarted at 14:05" stays in the status line.
const RESTARTED_SHOWN: Duration = Duration::from_secs(10 * 60);

/// One cloud's link, as the tray reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkState {
    /// No refresh token.
    NotLinked,
    /// A link is waiting for its code to be approved.
    Waiting {
        code: Option<String>,
    },
    /// A token, but the cloud can't be reached.
    Offline,
    /// The cloud answered `invalid_grant`: reads Offline until linked again or forgotten.
    UnlinkedOnTheWebsite,
    Connected,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudInput {
    /// "Production", "Staging", "Local" or "Custom".
    pub name: String,
    /// The cloud's URL, no trailing slash.
    pub url: String,
    pub host: String,
    /// While waiting: the `/link` page with the code filled in.
    pub link_page: Option<String>,
    pub link: LinkState,
}

/// Where a quit or a restart to load new code has got to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Exit {
    None,
    QuittingWhenIdle,
    RestartingForNewCode,
    /// Back after a restart: the local time it happened ("14:05") and how long ago.
    Restarted {
        at: String,
        ago: Duration,
    },
}

/// The shared backlog's approval, waiting for the person at the machine (installers.md section 4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BacklogWait {
    pub host: String,
    /// While its code waits: the `/link` page with it filled in.
    pub link_page: Option<String>,
    /// Unlinked on the store's agents page: "link again?" instead of "Approve".
    pub unlinked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayInput {
    pub clouds: Vec<CloudInput>,
    pub all_paused: bool,
    pub sessions: usize,
    pub has_project: bool,
    pub exit: Exit,
    /// Set while the shared backlog's approval waits for the person at the machine.
    pub backlog: Option<BacklogWait>,
    /// A release build: Open Retaskable, Start at login and Open the log.
    pub is_release: bool,
    /// Start at login is on (release build only).
    pub start_at_login: bool,
    /// Keep this computer awake: on or off, `None` when there is no way to hold it.
    pub keep_awake: Option<bool>,
    /// "Start with Windows" instead of "Start at login".
    pub windows: bool,
    pub machine: String,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconState {
    NotLinked,
    Waiting,
    Offline,
    Connected,
}

/// Section 4.1, following `trayState`: every linked cloud's state, worst first (Offline, Waiting, Connected);
/// Not linked only when none of them is.
pub fn icon_state(input: &TrayInput) -> IconState {
    let any = |f: fn(&LinkState) -> bool| input.clouds.iter().any(|c| f(&c.link));
    if any(|l| matches!(l, LinkState::Offline | LinkState::UnlinkedOnTheWebsite)) {
        IconState::Offline
    } else if any(|l| matches!(l, LinkState::Waiting { .. })) {
        IconState::Waiting
    } else if any(|l| matches!(l, LinkState::Connected)) {
        IconState::Connected
    } else {
        IconState::NotLinked
    }
}

pub(super) fn link_text(link: &LinkState) -> String {
    match link {
        LinkState::NotLinked => "not linked".into(),
        LinkState::Waiting { code } => match code {
            Some(code) => format!("waiting: {code}"),
            None => "waiting for approval".into(),
        },
        LinkState::Offline => "offline".into(),
        LinkState::UnlinkedOnTheWebsite => "unlinked on the website".into(),
        LinkState::Connected => "connected".into(),
    }
}

fn paused(input: &TrayInput) -> bool {
    input.has_project && input.all_paused
}

/// Section 4.6, following `trayTooltip`: "Retaskable · Production: connected · Local: offline", clouds not linked
/// left out (the first one shown when none is linked), " · Paused" while every project is, " · 1 waiting for you"
/// while the backlog waits.
pub fn tooltip(input: &TrayInput) -> String {
    let linked: Vec<&CloudInput> = input.clouds.iter().filter(|c| c.link != LinkState::NotLinked).collect();
    let shown = if linked.is_empty() {
        input.clouds.iter().take(1).collect()
    } else {
        linked
    };
    let mut parts = vec!["Retaskable".to_string()];
    parts.extend(shown.iter().map(|c| format!("{}: {}", c.name, link_text(&c.link))));
    if paused(input) {
        parts.push("Paused".into());
    }
    if input.backlog.is_some() {
        parts.push("1 waiting for you".into());
    }
    parts.join(" · ")
}

/// The menu's first line (disabled), following `statusLine`; "restarted at 14:05" ends the running, paused and
/// idle lines for 10 minutes after a restart.
pub fn status_line(input: &TrayInput) -> String {
    let s = input.sessions;
    let sessions = format!("{s} session{}", if s == 1 { "" } else { "s" });
    let restarted = match &input.exit {
        Exit::Restarted { at, ago } if *ago < RESTARTED_SHOWN => format!(" · restarted at {at}"),
        _ => String::new(),
    };
    let what = if input.exit == Exit::QuittingWhenIdle {
        let n = if s > 0 { format!(" ({sessions})") } else { String::new() };
        format!("quitting when idle{n}")
    } else if input.exit == Exit::RestartingForNewCode {
        "restarting for new code".into()
    } else if !input.has_project {
        "no project set up yet".into()
    } else if s > 0 {
        format!("running ({sessions}){restarted}")
    } else if paused(input) {
        format!("paused{restarted}")
    } else {
        format!("idle{restarted}")
    };
    format!("Retaskable · {what}")
}

/// The greyed-out line under it: "jcnash-pc4 · tags: windows, gpu, display" ("tags: none" with none), cut to a
/// menu title.
pub fn computer_line(input: &TrayInput) -> String {
    let tags = if input.tags.is_empty() {
        "none".to_string()
    } else {
        input.tags.join(", ")
    };
    clip(&format!("{} · tags: {tags}", input.machine), TITLE_MAX)
}
