//! Drawing a plan's designs on its idea's own board (TS `designBoard.ts` `drawDesignBoard`): reads the board, asks the
//! session for a layout, checks it, places it and writes the new things with their `chat` marks over the board's
//! socket. Pieces a person has touched are never changed (`place_layout` only adds, and takes away only the chat's
//! own untouched ones); a board with designs already keeps them beside the new ones, so a redraw is never laid over
//! the old. Refused (a [`DesignError`]) with the reason when it can't; the caller keeps it on the plan.
use futures_util::future::BoxFuture;
use serde_json::{Value, json};

use super::error::DesignError;
use super::kit::{board_layout_of, catalog_of};
use super::layout::{MAX_BOARD_PIECES, ResolvedLayout, check_layout};
use super::place::{own_ids, place_layout, read_board};
use super::session::{Idea, layout_prompt};
use super::socket::{IdeaBoard, Op, Timing, base36};
use crate::backlog::Client;
use crate::backlog::board::idea_board_id;
use crate::backlog_socket::Transport;
use crate::kit::Kit;
use crate::text::truncate;

/// Asks the layout session: the prompt in, the structured answer out, or why there is none (the project isn't set up,
/// Claude is signed out or at its limit, it fails). The caller wires it to the engine (`DraftHost::design_layout`).
pub type Layouts<'a> = &'a (dyn Fn(String) -> BoxFuture<'static, Result<Value, DesignError>> + Send + Sync);

/// Everything a drawing reaches: the store and its board, and the session.
pub struct Drawing<'a> {
    pub client: &'a Client,
    pub repo: u64,
    /// This computer's access token for the store.
    pub token: &'a str,
    pub transport: &'a dyn Transport,
    pub layouts: Layouts<'a>,
    pub timing: Timing,
    /// Claude is signed out on this computer: nothing is asked of it.
    pub claude_signed_out: bool,
    /// The project's kit (`kit::store::kit_for`): the closed sets a layout is checked against and the pieces it is drawn with.
    pub kit: &'a Kit,
}

/// What the session is refused with while Claude is signed out (`designSession`).
pub fn signed_out_reason() -> DesignError {
    DesignError::conflict(format!(
        "Claude is signed out on {}, so the designs can't be drawn until it's signed in again.",
        gethostname::gethostname().to_string_lossy()
    ))
}

/// What the session drew: its layout, checked, and the sentence it wrote for the "Does this look right?" prompt (not yet
/// checked: [`super::keep_review`] does that).
#[derive(Debug, Clone)]
pub struct Drawn {
    pub layout: ResolvedLayout,
    pub sentence: Value,
}

/// Asks the session for a layout of the idea's designs, checked, with the sentence it wrote (`designLayoutFor`): the sentence
/// is taken out of the answer before the layout is checked, as the layout's own shape has no place for it.
async fn layout_for(d: &Drawing<'_>, idea: &Idea<'_>, board: &super::place::BoardRead) -> Result<Drawn, DesignError> {
    if d.claude_signed_out {
        return Err(signed_out_reason());
    }
    let prompt = layout_prompt(idea, board, d.kit).map_err(|e| DesignError::conflict(format!("{e:#}")))?;
    let mut structured = (d.layouts)(prompt).await?;
    if !structured.is_object() {
        structured = json!({});
    }
    let sentence = structured
        .as_object_mut()
        .and_then(|o| o.remove("sentence"))
        .unwrap_or(Value::Null);
    match check_layout(&structured, &catalog_of(d.kit), &own_ids(board)) {
        Ok((layout, _notes)) => Ok(Drawn { layout, sentence }),
        Err(reason) => Err(DesignError::conflict(truncate(
            &format!("The layout didn't pass its check: {reason}"),
            280,
        ))),
    }
}

/// Draws plan `draft_id`'s designs on its board: the idea's id is the plan's store id (`ideaBoardId`). The person's
/// note when they asked for a redraw is `idea.change`.
pub async fn draw_design_board(d: &Drawing<'_>, draft_id: &str, idea: &Idea<'_>) -> Result<(), DesignError> {
    draw_design_board_with(d, draft_id, idea, |_| async {}).await
}

/// [`draw_design_board`], with `keep` told what was drawn as soon as the layout has passed its check, before anything is
/// written to the board: the review the plan's prompt shows is kept even when the board can't be opened or written (its window
/// needs only the stored layout, and the board is where **Change it…** goes), as the TS agent's `drawDesignBoard` does.
pub async fn draw_design_board_with<F, Fut>(
    d: &Drawing<'_>,
    draft_id: &str,
    idea: &Idea<'_>,
    keep: F,
) -> Result<(), DesignError>
where
    F: FnOnce(Drawn) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |t| t.as_millis() as u64);
    let opened = async {
        let ticket = d.client.board_ticket(d.repo, Some(d.token)).await?;
        let url = d.client.board_socket_url(d.repo, &ticket, &idea_board_id(draft_id));
        IdeaBoard::open(d.transport, &url, d.token, format!("agent-{}", base36(now)), d.timing).await
    }
    .await;
    let (mut board, board_error) = match opened {
        Ok(board) => (Some(board), None),
        Err(e) => (None, Some(e)),
    };
    let drawn = async {
        let things = board.as_ref().map_or(&[][..], |b| &b.things[..]);
        let drawn = layout_for(d, idea, &read_board(things, &[], MAX_BOARD_PIECES)).await?;
        let layout = drawn.layout.clone();
        keep(drawn).await;
        let Some(board) = board.as_mut() else {
            return Err(board_error.unwrap_or_else(|| DesignError::conflict("The board closed before it opened.")));
        };
        let tail: String = {
            let chars: Vec<char> = draft_id.chars().collect();
            chars[chars.len().saturating_sub(8)..].iter().collect()
        };
        let placed = place_layout(
            &board.things,
            &board_layout_of(&layout, d.kit),
            &format!("{tail}{}", base36(now)),
        );
        let mut ops: Vec<Op> = placed.removed.iter().cloned().map(Op::Remove).collect();
        ops.extend(
            placed
                .added
                .iter()
                .filter_map(|id| placed.things.iter().find(|t| t.id() == id))
                .cloned()
                .map(Op::Add),
        );
        board.write(&ops).await
    }
    .await;
    if let Some(board) = board {
        board.close().await;
    }
    drawn
}
