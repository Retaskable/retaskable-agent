//! Why a drawing can't be done, in words for the plan's card (`HttpError` in the TS agent: a status and a message).
use crate::text::truncate;

/// A refusal with its reason: `status` is the HTTP-like code a command's refusal carries (404 no such draft, 409
/// anything else).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesignError {
    pub status: u16,
    pub message: String,
}

impl DesignError {
    pub fn conflict(message: impl Into<String>) -> Self {
        Self {
            status: 409,
            message: message.into(),
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: 404,
            message: message.into(),
        }
    }

    /// The reason a drawing failed, in a few words for the plan's card (`designNoteOf`).
    pub fn note(&self) -> String {
        truncate(&self.message, 280)
    }
}

impl std::fmt::Display for DesignError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for DesignError {}

impl From<crate::backlog::BacklogError> for DesignError {
    fn from(e: crate::backlog::BacklogError) -> Self {
        Self::conflict(e.to_string())
    }
}
