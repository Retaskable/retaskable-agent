//! `claim` by name, as a person's action (Retaskable.com's `shared/backlogOps.ts` `claim`; `HostedWork.takeOver` in the
//! TS agent): another computer's item that waits on a person becomes this computer's. The store does every check
//! (who holds it, whether it waits, the owner's computer gone 2 hours) against its own current state and refuses in
//! its own words; this only says which refusal it was, in plain words naming who holds the item.
use serde::Deserialize;
use serde_json::{Value, json};

use super::{BacklogError, ClaimNextAgent, Client, Item};

/// What a person's claim by name answered: the item with a lease token, and what it was waiting for.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonClaim {
    pub item: Item,
    /// The lease this computer now holds.
    pub token: String,
    /// What the item was parked for (`ci`, `answer`, `tryout`, `merge`, `help`); `None` when it was not parked.
    #[serde(default)]
    pub was_waiting: Option<String>,
    /// Whose computer held it.
    #[serde(default)]
    pub was_owner: Option<Owner>,
}

/// Who held an item, from which computer.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Owner {
    #[serde(default)]
    pub login: Option<String>,
    #[serde(default)]
    pub machine: Option<String>,
}

/// Why the store refused a take-over, from its code and `why`: in words a person reads, never a path or a code.
pub fn refusal_words(code: Option<&str>, message: &str, why: Option<&Value>) -> String {
    let held = why.filter(|w| w.get("reason").and_then(Value::as_str) == Some("held"));
    let who = |key| held.and_then(|w| w.get(key)).and_then(Value::as_str);
    match (code, who("machine"), who("login")) {
        (Some("conflict"), Some(machine), Some(login)) => {
            format!(
                "@{login}'s computer ({machine}) is working on this right now. It'll come back to you if it needs you."
            )
        }
        (Some("held_by_owner"), Some(machine), Some(login)) => format!(
            "{machine} has been online in the last 2 hours, so it's still theirs. Carry it on, or ask @{login}."
        ),
        _ => format!("It can't be taken over in the shared backlog: {message}"),
    }
}

impl Client {
    /// `claim { id, agent, because: "person", did: "took_over" }`. A refusal comes back as plain words in `Err`,
    /// an unreachable store as its own words: nothing is claimed either way.
    pub async fn take_over(
        &self,
        repo_id: u64,
        token: Option<&str>,
        id: &str,
        agent: &ClaimNextAgent,
    ) -> Result<PersonClaim, String> {
        let args = json!({ "id": id, "agent": agent, "because": "person", "did": "took_over" });
        let (status, body) = self
            .send_why(repo_id, token, "claim", args)
            .await
            .map_err(|(e, why)| match &e {
                BacklogError::BadAnswer { code, message, .. } => refusal_words(code.as_deref(), message, why.as_ref()),
                BacklogError::Unreachable(_) => format!("Couldn't reach the shared backlog: {e}"),
            })?;
        let value = body
            .value
            .ok_or_else(|| format!("The shared backlog answered {status} with no item."))?;
        serde_json::from_value(value).map_err(|e| format!("Couldn't read the item it claimed: {e}"))
    }
}

#[cfg(test)]
mod tests;
