//! What the tray shows, as plain functions over what the agent knows (tray/04-the-tray-agent.md sections 4.1 and
//! 4.6): the icon's state, the tooltip, the status line and the greyed-out computer line. No I/O.
#[cfg(test)]
mod tests;

use std::time::Duration;

/// How long "restarted at 14:05" stays in the status line.
const RESTARTED_SHOWN: Duration = Duration::from_secs(10 * 60);

/// One cloud's link, as the tray reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkState {
    /// No refresh token.
    NotLinked,
    /// A link is waiting for its code to be approved.
    Waiting {
        code: String,
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
    pub host: String,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayInput {
    pub clouds: Vec<CloudInput>,
    pub all_paused: bool,
    pub sessions: usize,
    pub has_project: bool,
    pub exit: Exit,
    /// The shared backlog's approval waits for the person at the machine.
    pub backlog_waits: bool,
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

/// Section 4.1: the combination of the clouds' links, worst first. A waiting link comes first, since a cloud
/// waiting for its code has no token yet; then no link at all, then any cloud that can't be reached.
pub fn icon_state(input: &TrayInput) -> IconState {
    let links = || input.clouds.iter().map(|c| &c.link);
    if links().any(|l| matches!(l, LinkState::Waiting { .. })) {
        IconState::Waiting
    } else if links().all(|l| matches!(l, LinkState::NotLinked)) {
        IconState::NotLinked
    } else if links().any(|l| matches!(l, LinkState::Offline | LinkState::UnlinkedOnTheWebsite)) {
        IconState::Offline
    } else {
        IconState::Connected
    }
}

fn word(link: &LinkState) -> &'static str {
    match link {
        LinkState::NotLinked => "not linked",
        LinkState::Waiting { .. } => "waiting",
        LinkState::Offline | LinkState::UnlinkedOnTheWebsite => "offline",
        LinkState::Connected => "connected",
    }
}

/// Section 4.6: "Retaskable · Production: connected · Local: offline", clouds not linked left out (Production's
/// state when none is), " · Paused" while every project is, " · 1 waiting for you" while the backlog waits.
pub fn tooltip(input: &TrayInput) -> String {
    let mut out = String::from("Retaskable");
    let linked: Vec<&CloudInput> = input.clouds.iter().filter(|c| c.link != LinkState::NotLinked).collect();
    if linked.is_empty() {
        let first = input
            .clouds
            .iter()
            .find(|c| c.name == "Production")
            .or(input.clouds.first());
        let (name, link) = first.map_or(("Production", &LinkState::NotLinked), |c| (c.name.as_str(), &c.link));
        out.push_str(&format!(" · {name}: {}", word(link)));
    }
    for c in linked {
        out.push_str(&format!(" · {}: {}", c.name, word(&c.link)));
    }
    if input.all_paused {
        out.push_str(" · Paused");
    }
    if input.backlog_waits {
        out.push_str(" · 1 waiting for you");
    }
    out
}

/// The menu's first line (disabled).
pub fn status_line(input: &TrayInput) -> String {
    let what = match &input.exit {
        Exit::RestartingForNewCode => "restarting for new code".to_string(),
        Exit::QuittingWhenIdle => "quitting when idle".to_string(),
        Exit::Restarted { at, ago } if *ago < RESTARTED_SHOWN => format!("restarted at {at}"),
        _ if input.sessions > 0 => {
            let s = input.sessions;
            format!("running ({s} session{})", if s == 1 { "" } else { "s" })
        }
        _ if !input.has_project => "no project yet".to_string(),
        _ if input.all_paused => "paused".to_string(),
        _ => "idle".to_string(),
    };
    format!("Retaskable · {what}")
}

/// The greyed-out line under it: "jcnash-pc4 · tags: windows, gpu, display" (just the name with no tags).
pub fn computer_line(input: &TrayInput) -> String {
    if input.tags.is_empty() {
        input.machine.clone()
    } else {
        format!("{} · tags: {}", input.machine, input.tags.join(", "))
    }
}
