//! The design board (docs/specs/design-view/14-no-chat-an-idea-gets-its-own-design-board.md; Retaskable.com's
//! `agent/src/designBoard.ts`, `drafts.ts`): an idea whose change people will see (`needsDesign`) gets a whiteboard of
//! its own already holding the designs, and waits for the person to review them before **Approve plan**.
//!
//! - [`catalog`]: the closed sets a layout may use (the library's elements, the real pages and their parts), and how
//!   the prompt lists them.
//! - [`layout`]: the session's answer and its check, in plain code.
//! - [`things`], [`place`]: the board's things, what the session reads of them and a checked layout placed as the
//!   chat's own work (`who: "chat"`), never changing what a person touched.
pub mod catalog;
pub mod layout;
pub mod place;
#[cfg(test)]
mod place_tests;
pub mod things;
