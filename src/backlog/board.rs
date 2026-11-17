//! The store's board route (Retaskable.com's `agent/src/backlogStore.ts` `boardSocket`, docs/specs/whiteboard.md section
//! 28.4, design-view/14): `board_ticket` gives a one-use ticket, and `GET /v1/repos/<repo id>/board?ticket=&board=`
//! opens an idea's own design board (`board` is `idea-<idea id>`) with the store's access token in the
//! `Authorization` header, as the store's own socket takes it. The socket is the board's own, outbound only.
use serde::Deserialize;
use serde_json::json;

use super::{BacklogError, Client};

#[derive(Deserialize)]
struct Ticket {
    ticket: String,
}

/// The id an idea's own design board goes by (`ideaBoardId`, `shared/boardDoc.ts`): the same board for everyone.
pub fn idea_board_id(idea_id: &str) -> String {
    format!("idea-{idea_id}")
}

/// Percent-encoding for a query value (`encodeURIComponent`: everything but letters, digits and `-_.!~*'()`).
fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

impl Client {
    /// `board_ticket {}`: a ticket that opens a board socket once.
    pub async fn board_ticket(&self, repo_id: u64, token: Option<&str>) -> Result<String, BacklogError> {
        let t: Ticket = self
            .op(repo_id, token, "board_ticket", json!({}), "board ticket")
            .await?;
        Ok(t.ticket)
    }

    /// Where `board`'s socket is, opened with `ticket`: the store's address as a `ws` one.
    pub fn board_socket_url(&self, repo_id: u64, ticket: &str, board: &str) -> String {
        format!(
            "{}/v1/repos/{repo_id}/board?ticket={}&board={}",
            self.base_url.replacen("http", "ws", 1),
            encode(ticket),
            encode(board)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_socket_url_is_the_stores_with_a_ws_scheme_and_an_encoded_query() {
        let c = Client::new("https://store.example");
        assert_eq!(
            c.board_socket_url(7, "a b/c", "idea-x1"),
            "wss://store.example/v1/repos/7/board?ticket=a%20b%2Fc&board=idea-x1"
        );
        let local = Client::new("http://127.0.0.1:8787");
        assert!(
            local
                .board_socket_url(1, "t", "idea-a")
                .starts_with("ws://127.0.0.1:8787/v1/repos/1/board?")
        );
        assert_eq!(idea_board_id("draft-abc"), "idea-draft-abc");
    }
}
