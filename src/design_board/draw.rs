//! Drawing a plan's designs on its idea's own board (TS `designBoard.ts` `drawDesignBoard`): reads the board, asks the
//! session for a layout, checks it, places it and writes the new things with their `chat` marks over the board's
//! socket. Pieces a person has touched are never changed (`place_layout` only adds, and takes away only the chat's
//! own untouched ones); a board with designs already keeps them beside the new ones, so a redraw is never laid over
//! the old. Refused (a [`DesignError`]) with the reason when it can't; the caller keeps it on the plan.
use futures_util::future::BoxFuture;
use serde_json::Value;

use super::catalog::design_catalog;
use super::error::DesignError;
use super::layout::{MAX_BOARD_PIECES, check_layout};
use super::place::{own_ids, place_layout, read_board};
use super::session::{Idea, layout_prompt};
use super::socket::{IdeaBoard, Op, Timing, base36};
use crate::backlog::Client;
use crate::backlog::board::idea_board_id;
use crate::backlog_socket::Transport;
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
}

/// What the session is refused with while Claude is signed out (`designSession`).
pub fn signed_out_reason() -> DesignError {
    DesignError::conflict(format!(
        "Claude is signed out on {}, so the designs can't be drawn until it's signed in again.",
        gethostname::gethostname().to_string_lossy()
    ))
}

/// Asks the session for a layout of the idea's designs, checked (`designLayoutFor`).
async fn layout_for(
    d: &Drawing<'_>,
    idea: &Idea<'_>,
    board: &super::place::BoardRead,
) -> Result<super::layout::ResolvedLayout, DesignError> {
    if d.claude_signed_out {
        return Err(signed_out_reason());
    }
    let prompt = layout_prompt(idea, board).map_err(|e| DesignError::conflict(format!("{e:#}")))?;
    let structured = (d.layouts)(prompt).await?;
    match check_layout(&structured, &design_catalog(), &own_ids(board)) {
        Ok((layout, _notes)) => Ok(layout),
        Err(reason) => Err(DesignError::conflict(truncate(
            &format!("The layout didn't pass its check: {reason}"),
            280,
        ))),
    }
}

/// Draws plan `draft_id`'s designs on its board: the idea's id is the plan's store id (`ideaBoardId`). The person's
/// note when they asked for a redraw is `idea.change`.
pub async fn draw_design_board(d: &Drawing<'_>, draft_id: &str, idea: &Idea<'_>) -> Result<(), DesignError> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |t| t.as_millis() as u64);
    let ticket = d.client.board_ticket(d.repo, Some(d.token)).await?;
    let url = d.client.board_socket_url(d.repo, &ticket, &idea_board_id(draft_id));
    let mut board = IdeaBoard::open(d.transport, &url, d.token, format!("agent-{}", base36(now)), d.timing).await?;
    let drawn = async {
        let layout = layout_for(d, idea, &read_board(&board.things, &[], MAX_BOARD_PIECES)).await?;
        let tail: String = {
            let chars: Vec<char> = draft_id.chars().collect();
            chars[chars.len().saturating_sub(8)..].iter().collect()
        };
        let placed = place_layout(&board.things, &layout, &format!("{tail}{}", base36(now)));
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
    board.close().await;
    drawn
}
