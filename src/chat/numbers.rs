//! The Chat Agent's numbers: the store's, read from `settings_get`'s `effective.tuning` (pools-on-server.md section 18.7).
//! The agent holds none of its own; without them it sorts, files and sends nothing (fail closed).
use std::time::Duration;

use serde_json::{Value, json};

use super::board::Board;

/// `chatConfidencePct`, `chatMaxAsks` and `codeQuestionWaitMinutes`, each the project's value or the store's default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChatNumbers {
    /// A bug, idea or code question at or above this percent is filed or sent on; anything else is answered as a question.
    pub confidence_pct: u32,
    /// Asks carried out at once; the next is refused.
    pub max_asks: usize,
    /// How long a code question waits for a Coding Agent's answer before it says none was free.
    pub code_wait: Duration,
}

impl ChatNumbers {
    /// The numbers of a `settings_get` answer, or `None` when any is missing or is not a whole number.
    pub fn read(settings: &Value) -> Option<Self> {
        let tuning = settings.get("effective")?.get("tuning")?;
        let n = |key: &str| tuning.get(key)?.as_u64().and_then(|v| u32::try_from(v).ok());
        Some(Self {
            confidence_pct: n("chatConfidencePct")?,
            max_asks: usize::try_from(n("chatMaxAsks")?).ok()?,
            code_wait: Duration::from_secs(u64::from(n("codeQuestionWaitMinutes")?) * 60),
        })
    }
}

/// Reads the numbers from the store; `None` when it can't be reached or doesn't send them.
pub async fn fetch<B: Board>(board: &B) -> Option<ChatNumbers> {
    ChatNumbers::read(&board.op("settings_get", json!({})).await.ok()?)
}

#[cfg(test)]
pub(crate) fn test_settings() -> Value {
    json!({ "values": {}, "effective": { "tuning": {
        "chatConfidencePct": 80, "chatMaxAsks": 4, "codeQuestionWaitMinutes": 5
    } } })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_numbers_are_the_stores_and_any_missing_one_is_none() {
        let n = ChatNumbers::read(&test_settings()).unwrap();
        assert_eq!(
            (n.confidence_pct, n.max_asks, n.code_wait),
            (80, 4, Duration::from_secs(300))
        );
        for gone in ["chatConfidencePct", "chatMaxAsks", "codeQuestionWaitMinutes"] {
            let mut s = test_settings();
            s["effective"]["tuning"].as_object_mut().unwrap().remove(gone);
            assert_eq!(ChatNumbers::read(&s), None, "{gone}");
        }
        assert_eq!(ChatNumbers::read(&json!({ "values": {} })), None);
        let mut s = test_settings();
        s["effective"]["tuning"]["chatMaxAsks"] = json!("four");
        assert_eq!(ChatNumbers::read(&s), None);
    }
}
