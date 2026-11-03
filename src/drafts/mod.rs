//! The spec drafter (docs/specs/agent/06.5-spec-drafts.md): the session that turns a request into a
//! spec change a person approves. Retaskable.com's `agent/src/drafts.ts` is the ground truth. This
//! piece is the drafter's answer ([`model`]: its result, schema and the breakdown check) and its
//! prompt ([`prompt`]).
pub mod actions;
pub mod files;
pub mod model;
pub mod prompt;
pub mod session;

pub use model::{Breakdown, BuildPiece, DrafterResult, DrafterStatus, Fix, PeopleStep, Piece, drafter_result_schema};
pub use prompt::{build_prompt, titles_text};
