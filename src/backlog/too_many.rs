//! A `429` from the store and the wait its `Retry-After` names.
use super::BacklogError;

/// What a `429`'s message carries of the store's `Retry-After` (seconds), so the wait reaches whoever asked.
const RETRY_AFTER_MARK: &str = " (retry after ";

impl BacklogError {
    /// A `429` with the wait the store's `Retry-After` header named, if it named one.
    pub fn too_many(message: &str, retry_after: Option<std::time::Duration>) -> Self {
        let message = match retry_after {
            Some(w) => format!("{message}{RETRY_AFTER_MARK}{}s)", w.as_secs()),
            None => message.to_string(),
        };
        BacklogError::BadAnswer {
            status: 429,
            code: None,
            message,
        }
    }

    /// How long the store asked to wait before asking again: the `Retry-After` of a `429`.
    pub fn retry_after(&self) -> Option<std::time::Duration> {
        let BacklogError::BadAnswer {
            status: 429, message, ..
        } = self
        else {
            return None;
        };
        let rest = &message[message.rfind(RETRY_AFTER_MARK)? + RETRY_AFTER_MARK.len()..];
        rest.strip_suffix("s)")?
            .parse()
            .ok()
            .map(std::time::Duration::from_secs)
    }
}
