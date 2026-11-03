//! Retaskable's own messages in the chat (docs/specs/agent/06.6-ask-claude.md, "Retaskable's own messages"). They are
//! written here, in code, never by a model.

/// A draft a person asked for is written and waits for their yes.
pub const WRITTEN: &str = "The spec is written. Do you want to approve it?";

/// The drafter needs answers before it can write the spec.
pub const HAS_QUESTIONS: &str = "The drafter has questions before it can write this up. Can you answer them?";
