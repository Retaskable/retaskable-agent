//! The draft specs a turn asks to file (docs/specs/expert-chat.md section 5a; Retaskable.com's `specsAsked` in
//! `agent/src/expert/turns.ts`): each a request for a draft on the session's project, filed after the turn's summary.
use serde_json::{Value, json};

use crate::text::clip;

/// The most drafts one turn files, and the longest request (the store's own limit).
pub const ASKED_MAX: usize = 20;

/// The schema's `specs` property.
pub fn property() -> Value {
    json!({
        "type": "array",
        "description": "Only when the person asks you to send work to Retaskable (specs, tickets, ideas): each a request for a draft spec on this project, which Retaskable files as a draft for the person to approve on the site. Write each one so it stands alone. This is the way to send it: there is no other tool for it.",
        "items": {
            "type": "object",
            "properties": { "request": { "type": "string" } },
            "required": ["request"],
            "additionalProperties": false
        }
    })
}

/// The requests a turn's structured result asks to file: trimmed, the empty ones dropped, each cut to the request
/// limit, at most twenty.
pub fn asked(structured: &Value) -> Vec<String> {
    (structured.get("specs").and_then(Value::as_array).into_iter().flatten())
        .filter_map(|s| {
            let request = s.get("request")?.as_str()?.trim();
            (!request.is_empty()).then(|| clip(request, super::DRAFT_REQUEST_MAX))
        })
        .take(ASKED_MAX)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_trimmed_cut_and_capped() {
        let got = asked(&json!({ "specs": [
            { "request": "  Piece one  " }, { "request": "" }, { "request": "   " }, { "nope": 1 },
            { "request": "x".repeat(25_000) },
        ] }));
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], "Piece one");
        assert_eq!(got[1].chars().count(), 20_000);
        let many = json!({ "specs": (0..30).map(|i| json!({ "request": format!("r{i}") })).collect::<Vec<_>>() });
        assert_eq!(asked(&many).len(), ASKED_MAX);
        assert!(asked(&json!({})).is_empty() && asked(&json!({ "specs": "x" })).is_empty());
    }
}
