//! Retaskable's own messages in the chat (spec 06.6, "Retaskable's own messages"): written in code, never by a model.

/// A draft that is written and waits for a person's yes.
pub const DRAFT_READY: &str = "The spec is written. Do you want to approve it?";

/// A draft whose drafter has questions for the person.
pub const DRAFT_QUESTIONS: &str = "I have questions before I can write the spec. Can you answer them?";
