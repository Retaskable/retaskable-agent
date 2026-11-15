//! A hosted lease's completion as one idempotent store write, and the item details a holder reports
//! (docs/specs/agent/07.5-what-a-claim-and-a-give-back-send.md, Retaskable.com's
//! backlog/16-completing-is-one-store-write.md). `complete` is keyed by the lease token: it counts the slice, sets
//! what is left, builds the item or puts it back in the queue, and keeps the slice's suggested playtest as the item's
//! `implPlaytest` detail. The same token sent again changes nothing and answers what the first answered.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::{BacklogError, Client};

/// A slice's suggested playtest (`{ what, steps }`), as `complete` carries it and the item's details keep it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Playtest {
    pub what: String,
    #[serde(default)]
    pub steps: Vec<String>,
}

/// What a `complete` call carries. `remaining` present (even empty) is the slice's merge: the store counts it. A
/// call without it does not count a slice again (an older agent's, after `report { merged }`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Completion {
    /// The item is built: its change is on the base branch or the project's branch. Needs a `merge_commit`.
    pub built: bool,
    pub merge_commit: Option<String>,
    pub on_default_branch: bool,
    pub remaining: Option<String>,
    pub playtest: Option<Playtest>,
}

impl Completion {
    fn args(&self, id: &str, lease: &str) -> Value {
        let mut args = json!({ "id": id, "token": lease, "built": self.built });
        if let Some(sha) = &self.merge_commit {
            args["mergeCommit"] = json!(sha);
        }
        if self.on_default_branch {
            args["onDefaultBranch"] = json!(true);
        }
        if let Some(remaining) = &self.remaining {
            args["remaining"] = json!(remaining);
        }
        if let Some(playtest) = &self.playtest {
            args["playtest"] = json!(playtest);
        }
        args
    }
}

/// What `complete` answers: whether the item is built, and the status it is in now. A repeat of the same token answers
/// the same.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Completed {
    pub built: bool,
    pub status: String,
}

/// An item's details record as `details_report` answers it: the version the next report names as its `baseVersion`, and
/// the details as the store holds them.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct DetailsRecord {
    pub version: u64,
    #[serde(default)]
    pub details: Value,
}

/// A `details_report`: a patch of only the changed fields over `base_version`, with the lease's token (or none, as
/// the owner of a parked item).
#[derive(Debug, Clone, PartialEq)]
pub struct DetailsReport<'a> {
    pub id: &'a str,
    pub lease: Option<&'a str>,
    pub base_version: u64,
    pub patch: Map<String, Value>,
    /// A plain sentence for the item's history, which a `report` cannot carry once the item is parked.
    pub note: Option<&'a str>,
}

impl Client {
    /// `complete { id, token, built, mergeCommit?, onDefaultBranch?, remaining?, playtest? }`. Idempotent by the lease:
    /// sending the same token again is answered as the first send was; a token the item has moved on from is a
    /// `stale_lease` refusal, and the same token with another merge commit is `conflict`.
    pub async fn complete(
        &self,
        repo_id: u64,
        token: Option<&str>,
        id: &str,
        lease: &str,
        completion: &Completion,
    ) -> Result<Completed, BacklogError> {
        let args = completion.args(id, lease);
        self.op(repo_id, token, "complete", args, "completion answer").await
    }

    /// `details_report { id, token?, baseVersion, patch, note? }`: the new record. A `conflict` refusal means the
    /// details changed since `baseVersion`; its message holds the current record, so read them again.
    pub async fn details_report(
        &self,
        repo_id: u64,
        token: Option<&str>,
        report: &DetailsReport<'_>,
    ) -> Result<DetailsRecord, BacklogError> {
        let mut args = json!({ "id": report.id, "baseVersion": report.base_version, "patch": report.patch });
        if let Some(lease) = report.lease {
            args["token"] = json!(lease);
        }
        if let Some(note) = report.note {
            args["note"] = json!(note);
        }
        self.op(repo_id, token, "details_report", args, "details record").await
    }

    /// The suggested playtest goes up with the details, as `{ implPlaytest }`, when the slice's change opens, so a
    /// restart reads it from the store.
    pub async fn report_impl_playtest(
        &self,
        repo_id: u64,
        token: Option<&str>,
        id: &str,
        lease: &str,
        base_version: u64,
        playtest: &Playtest,
    ) -> Result<DetailsRecord, BacklogError> {
        let mut patch = Map::new();
        patch.insert("implPlaytest".into(), json!(playtest));
        let report = DetailsReport {
            id,
            lease: Some(lease),
            base_version,
            patch,
            note: None,
        };
        self.details_report(repo_id, token, &report).await
    }
}

#[cfg(test)]
mod tests;
