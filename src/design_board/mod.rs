//! The design board (docs/specs/design-view/14-no-chat-an-idea-gets-its-own-design-board.md; Retaskable.com's
//! `agent/src/designBoard.ts`, `drafts.ts`): an idea whose change people will see (`needsDesign`) gets a whiteboard of
//! its own already holding the designs, and waits for the person to review them before **Approve plan**.
//!
//! - [`catalog`]: the closed sets a layout may use (the library's elements, the real pages and their parts), and how
//!   the prompt lists them.
//! - [`layout`]: the session's answer and its check, in plain code.
//! - [`things`], [`place`]: the board's things, what the session reads of them and a checked layout placed as the
//!   chat's own work (`who: "chat"`), never changing what a person touched.
//! - [`session`], [`socket`], [`draw`]: the read-only session that answers the layout, the idea's board socket and
//!   the drawing that joins them.
//! - [`needs_design`]: the plain-code test of whether a plan needs the review (and the flag as the store keeps it).
//! - [`act`]: what **Skip the design** and **Try again** / **Change it…** do (the command arms call these).
pub mod act;
pub mod catalog;
pub mod draw;
#[cfg(test)]
mod draw_tests;
pub mod error;
pub mod keep;
pub mod layout;
pub mod needs_design;
pub mod place;
#[cfg(test)]
mod place_tests;
pub mod review;
pub mod sentence;
pub mod session;
pub mod socket;
pub mod things;

pub use act::{Acting, draw_design, skip_design};
pub use draw::{Drawing, Drawn, Layouts, draw_design_board, draw_design_board_with};
pub use error::DesignError;
pub use keep::{Keeping, keep_review};
pub use needs_design::{NEEDS_DESIGN_FIRST, design_note, design_pending};
