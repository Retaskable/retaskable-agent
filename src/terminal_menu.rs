//! The terminal menu's pure half (tray/04-the-tray-agent.md section 4.7), following `agent/src/terminalMenu.ts`:
//! `render_screen` turns the tray's menu model, a cursor, the terminal's size and the console's last lines into
//! screen lines (plain ANSI), and `handle_key` turns a key into a new cursor and what to do. The cursor holds item
//! ids, not positions, so it stays on the same item as the menu changes.
pub mod console;
pub mod keys;
#[cfg(test)]
mod tests;

use crate::tray::menu::{Item, Menu, SEPARATOR};

/// The header's status line is the menu's first item; the tooltip comes from the caller.
const STATUS: &str = "status";
/// Highlighted: the shared backlog's wait.
const HIGHLIGHTED: [&str; 2] = ["requests", "request.backlog"];
const BACKLOG: &str = "request.backlog";
pub const QUIT_HINT: &str = "Choose Quit or Quit when idle, or press Ctrl+C, to stop the agent.";

const DIM: &str = "2";
const BOLD: &str = "1";
const ALERT: &str = "1;33";
const REVERSE: &str = "7";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    Enter,
    Esc,
    Backspace,
    CtrlC,
    Char(char),
}

/// Where the person is: the ids of the open submenus, outermost first, and the item under the cursor in the
/// innermost list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cursor {
    pub path: Vec<String>,
    pub on: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Run the item with this id.
    Run(String),
    /// Ctrl+C: stop the agent.
    Stop,
    /// `q`: say to choose Quit or press Ctrl+C ([`QUIT_HINT`]).
    Hint,
}

fn choosable(i: &Item) -> bool {
    !i.hidden && i.enabled && (!i.children.is_empty() || i.action.is_some())
}

/// The open submenus (cut where the menu no longer has one) and the visible items of the innermost list.
fn level<'a>(menu: &'a Menu, path: &[String]) -> (Vec<&'a Item>, Vec<&'a Item>) {
    let mut list = &menu.items[..];
    let mut open = vec![];
    for id in path {
        match list.iter().find(|i| i.id == *id && !i.hidden && !i.children.is_empty()) {
            Some(i) => {
                open.push(i);
                list = &i.children;
            }
            None => break,
        }
    }
    let top = open.is_empty();
    let shown = list.iter().filter(|i| !i.hidden && !(top && i.id == STATUS)).collect();
    (open, shown)
}

/// The cursor made to fit the menu as it is now: submenus that are gone are left, and an item that is gone or
/// can't be chosen any more is replaced by the first that can.
fn fit<'a>(menu: &'a Menu, cursor: &Cursor) -> (Cursor, Vec<&'a Item>) {
    let (open, shown) = level(menu, &cursor.path);
    let on = cursor
        .on
        .iter()
        .find(|id| shown.iter().any(|i| i.id == **id && choosable(i)))
        .cloned()
        .or_else(|| shown.iter().find(|i| choosable(i)).map(|i| i.id.clone()));
    let path = open.iter().map(|i| i.id.clone()).collect();
    (Cursor { path, on }, shown)
}

pub fn handle_key(menu: &Menu, cursor: &Cursor, key: Key) -> (Cursor, Option<Outcome>) {
    let (cur, shown) = fit(menu, cursor);
    let stops: Vec<&Item> = shown.iter().copied().filter(|i| choosable(i)).collect();
    let at = cur.on.as_ref().and_then(|id| stops.iter().position(|i| i.id == *id));
    let go = |to: Option<usize>| {
        let on = to.and_then(|n| stops.get(n)).map(|i| i.id.clone()).or(cur.on.clone());
        (
            Cursor {
                path: cur.path.clone(),
                on,
            },
            None,
        )
    };
    let selected = || at.map(|n| stops[n]);
    let open = |item: &Item| {
        let mut path = cur.path.clone();
        path.push(item.id.clone());
        let on = item.children.iter().find(|c| choosable(c)).map(|c| c.id.clone());
        (Cursor { path, on }, None)
    };
    let back = || {
        let mut path = cur.path.clone();
        let on = path.pop();
        (
            Cursor {
                path,
                on: on.or(cur.on.clone()),
            },
            None,
        )
    };
    match key {
        Key::CtrlC => (cur, Some(Outcome::Stop)),
        Key::Char('q') => (cur, Some(Outcome::Hint)),
        Key::Up | Key::Char('k') => go(at.map(|n| n.saturating_sub(1))),
        Key::Down | Key::Char('j') => go(at.map(|n| (n + 1).min(stops.len() - 1))),
        Key::Home => go(Some(0)),
        Key::End => go(stops.len().checked_sub(1)),
        Key::Enter | Key::Char(' ') => match selected() {
            Some(i) if !i.children.is_empty() => open(i),
            Some(i) => (cur.clone(), Some(Outcome::Run(i.id.clone()))),
            None => (cur, None),
        },
        Key::Right | Key::Char('l') => match selected() {
            Some(i) if !i.children.is_empty() => open(i),
            _ => (cur, None),
        },
        Key::Left | Key::Esc | Key::Char('h') | Key::Backspace => {
            if cur.path.is_empty() {
                (cur, None)
            } else {
                back()
            }
        }
        Key::Char(_) => (cur, None),
    }
}

/// The line under the header while the shared backlog's approval waits, if it does.
pub fn backlog_notice(menu: &Menu) -> Option<String> {
    let b = menu.items.iter().find(|i| i.id == BACKLOG && !i.hidden)?;
    let mut title = b.text.chars();
    let title: String = title.next()?.to_lowercase().chain(title).collect();
    let who = b.children.first().map(|c| c.text.as_str()).unwrap_or_default();
    Some(format!("! Retaskable: {title}: {who}"))
}

fn cut(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        s.to_string()
    } else if width == 0 {
        String::new()
    } else {
        s.chars().take(width - 1).chain(['…']).collect()
    }
}

fn styled(text: &str, width: usize, code: Option<&str>) -> String {
    let t = cut(text, width);
    match code {
        Some(c) => format!("\x1b[{c}m{t}\x1b[0m"),
        None => t,
    }
}

/// The screen: the header (the status line, then the tooltip's line), the notice, the menu (or the open submenu) scrolled to keep the cursor in sight, then
/// the last of the console that fits. At most `height` lines, each cut to `width`.
pub fn render_screen(
    menu: &Menu,
    cursor: &Cursor,
    width: usize,
    height: usize,
    tooltip: &str,
    notice: Option<&str>,
    log_lines: &[String],
) -> Vec<String> {
    let (cur, shown) = fit(menu, cursor);
    let (open, _) = level(menu, &cur.path);
    let text_of = |id: &str| {
        menu.items
            .iter()
            .find(|i| i.id == id && !i.hidden)
            .map(|i| i.text.as_str())
    };
    let mut top = vec![styled(text_of(STATUS).unwrap_or_default(), width, Some(BOLD))];
    if !tooltip.is_empty() {
        top.push(styled(tooltip, width, Some(DIM)));
    }
    if let Some(n) = notice {
        top.push(styled(n, width, Some(ALERT)));
    }
    if !open.is_empty() {
        let path: Vec<&str> = open.iter().map(|i| i.text.as_str()).collect();
        top.push(styled(&format!("‹ {}", path.join(" / ")), width, Some(BOLD)));
    }

    let mut cursor_row = 0;
    let rows: Vec<String> = shown
        .iter()
        .enumerate()
        .map(|(n, i)| {
            if i.text == SEPARATOR {
                return "─".repeat(width);
            }
            let here = cur.on.as_deref() == Some(i.id.as_str());
            if here {
                cursor_row = n;
            }
            let tick = if i.checked { "✓ " } else { "" };
            let arrow = if i.children.is_empty() { "" } else { " ▸" };
            let text = format!("{}{tick}{}{arrow}", if here { "› " } else { "  " }, i.text);
            let code = if here {
                Some(REVERSE)
            } else if !i.enabled && !HIGHLIGHTED.contains(&i.id.as_str()) {
                Some(DIM)
            } else if HIGHLIGHTED.contains(&i.id.as_str()) {
                Some(ALERT)
            } else {
                None
            };
            styled(&text, width, code)
        })
        .collect();

    let room = height.saturating_sub(top.len());
    let body = rows.len().min(room);
    let from = (cursor_row + 1).saturating_sub(body);
    let mut out = top;
    out.extend(rows.into_iter().skip(from).take(body));
    let rest = room - body;
    if rest >= 2 && !log_lines.is_empty() {
        out.push("─".repeat(width));
        let take = (rest - 1).min(log_lines.len());
        out.extend(log_lines[log_lines.len() - take..].iter().map(|l| cut(l, width)));
    }
    out.truncate(height);
    out
}
