//! The store's draft ops (docs/specs/agent/06.5-spec-drafts.md; Retaskable.com's team-state-on-server.md section
//! 3.3 and spec-queue.md): `draft_claim_next`, `draft_report`, `draft_answer_taken`, `draft_front`,
//! `upsert_from_specs` and the `add` of a chat fix. A claim's token is the draft's lease, sent back by `draft_report`
//! and `draft_answer_taken`. Every failure is an error, never an empty answer.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::{BacklogError, ClaimNextAgent, Client};
use crate::spec_items::SpecItem;

/// A person's answers to a draft's questions, one per question in order.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftAnswer {
    #[serde(default)]
    pub answers: Vec<String>,
    #[serde(default)]
    pub by: Option<String>,
}

/// What a queued draft carries to the computer that writes it: the chat's parse of the request.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftIntake {
    /// The theme of each parsed title, in the titles' order.
    #[serde(default)]
    pub themes: Vec<String>,
    #[serde(default)]
    pub kinds: Vec<String>,
    #[serde(default)]
    pub trial: bool,
    #[serde(default)]
    pub parsed: bool,
    #[serde(default)]
    pub note: Option<String>,
}

/// The draft `draft_claim_next` hands out.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimedDraft {
    pub id: String,
    /// The issue a plan request came from (`{ provider, key, url }`), for an imported one: its text is quoted.
    #[serde(default)]
    pub issue: Option<Value>,
    /// The request's text.
    #[serde(default)]
    pub request: Option<String>,
    /// The titles the chat parsed the request into.
    #[serde(default)]
    pub titles: Vec<String>,
    #[serde(default)]
    pub intake: Option<DraftIntake>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub answer: Option<DraftAnswer>,
    #[serde(default)]
    pub rebase_reason: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub pr_url: Option<String>,
    /// The chat ask the draft was asked for in (none for Add spec and feedback drafts).
    #[serde(default)]
    pub ask_id: Option<String>,
    /// The drafter's block for the item the plan revises, set by the draft step before the session starts
    /// (`drafts::revision::revision_text`); never read from the store.
    #[serde(skip)]
    pub revising: Option<String>,
    /// Any other field of the draft.
    #[serde(flatten)]
    pub other: Map<String, Value>,
}

/// A claimed draft and the token that leases it to this computer.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct DraftClaim {
    pub draft: ClaimedDraft,
    pub token: String,
}

/// A draft taken for a person's press (`draft_take`): the store's draft and the lease that goes with it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct TakenDraft {
    pub draft: ClaimedDraft,
    pub token: String,
}

/// What `draft_report` changes: only the fields given are sent.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftReport {
    /// The store's statuses: `queued`, `drafting`, `needs_person`, `merged`, `discarded`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// The holder's exact state beside the status: `awaiting_approval`, `needs_clarification`, `failed`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The drafter's session, so a restart resumes it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// A failed draft put back in the queue by itself, once.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retried: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub titles: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub questions: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,
    /// Every backlog entry the draft added, in order.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item_ids: Option<Vec<String>>,
    /// The drafter's checked breakdown, as the store's details take it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub breakdown: Option<Value>,
    /// The item the plan revises: `Some(None)` sends `null`, which clears it (the plan is new work after all).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revises: Option<Option<String>>,
    /// What is left of that item once the change is in: `Some(None)` sends `null`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<Option<Remaining>>,
    /// The plan waits for a design review before **Approve plan** (design-view.md section 11): `true` when it is
    /// written, `false` once **Skip the design** or **Approve design** ends the review.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub needs_design: Option<bool>,
    /// Why the designs weren't drawn, for the plan's card; `Some(None)` sends `null`, which clears it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub design_note: Option<Option<String>>,
    /// The facts of a plan ready for approval, which the store decides from (pools-on-server.md section 18.2): whether it goes ahead on
    /// the yes given when it was asked for and whether it waits for a design review. Sent with that report only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<PlanFacts>,
}

/// One build step of a plan's breakdown, as the store's `tierOf` reads it (`FactPiece`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FactPiece {
    pub size: String,
    /// The item's kind (`tweak` or `feature`), when its spec says one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The plan's own call on who checks it (`manual`, ...); the drafter here gives none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checking: Option<String>,
}

/// The facts of a plan the drafter just wrote (`PlanFactsSent` in shared/planFacts.ts). The agent holds no rule for them and works out
/// neither the tier nor the drafting table: the store does, from `pieces`, `people` and `paths`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanFacts {
    /// The drafter said the plan is risky.
    pub risk: bool,
    /// Each build step of the breakdown, with its size and its item's kind.
    pub pieces: Vec<FactPiece>,
    /// Each people step of the breakdown: whether a piece waits on it.
    pub people: Vec<PeopleBlocks>,
    /// The files the plan changes and every item's `specRefs`.
    pub paths: Vec<String>,
    /// The steps in its breakdown that build something: 0 when it has no breakdown.
    pub build: usize,
    pub title: String,
    /// The kinds of the items it adds, when each is a tweak or a feature (none otherwise).
    pub item_kinds: Vec<String>,
    /// One of the files it adds is a preview that is safe to draw.
    pub preview: bool,
    /// This is the first time it is written: a plan already written and sent back is not moved to a design review again.
    pub first: bool,
}

/// A people step of a breakdown, as the tier rule reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PeopleBlocks {
    pub blocks: bool,
}

/// What the store answers a report that carried [`PlanFacts`]: the decision it made from them.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PlanDecision {
    /// The plan goes ahead on the yes given when it was asked for.
    pub approve: bool,
    /// The plan waits for a design review, and the designs are to be drawn.
    pub needs_design: bool,
    /// It is on a drafting table (the store works it out).
    pub table: bool,
    /// Its tier (`routine`, `tryfirst` or `needsyou`) for the project's risk profile and why; the store's, never worked out here.
    /// `None` when it sent none: the careful `needsyou` ([`PlanDecision::tier_or_careful`]).
    pub tier: Option<String>,
    pub reasons: Vec<String>,
}

impl PlanDecision {
    /// The store's answer, or `None` when it said nothing about the plan (an older store, or one that was not asked): the plan then
    /// waits for a person.
    /// The plan's tier and why, or, when the store sent none, the careful tier (`needsyou`) and a reason saying so.
    pub fn tier_or_careful(&self) -> (&str, Vec<String>) {
        match &self.tier {
            Some(t) => (t, self.reasons.clone()),
            None => (
                "needsyou",
                vec!["the store could not say how much a person has to be in the loop".to_string()],
            ),
        }
    }

    /// [`Self::tier_or_careful`], owned.
    pub fn tier_or_careful_owned(&self) -> (String, Vec<String>) {
        let (t, r) = self.tier_or_careful();
        (t.to_string(), r)
    }

    fn of(answer: Option<&Value>) -> Option<Self> {
        let a = answer?.as_object()?;
        Some(Self {
            approve: a.get("approve")?.as_bool()?,
            needs_design: a.get("needsDesign").and_then(Value::as_bool) == Some(true),
            table: a.get("table").and_then(Value::as_bool) == Some(true),
            tier: a
                .get("tier")
                .and_then(Value::as_str)
                .filter(|t| matches!(*t, "routine" | "tryfirst" | "needsyou"))
                .map(str::to_string),
            reasons: (a.get("reasons").and_then(Value::as_array).into_iter().flatten())
                .filter_map(|r| r.as_str().map(str::to_string))
                .collect(),
        })
    }
}

/// The longest `designNote` the store keeps (`DESIGN_NOTE_MAX`).
const DESIGN_NOTE_MAX: usize = 500;

/// A revision as `draft_report` takes it (`{ remaining }`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Remaining {
    pub remaining: String,
}

/// A fix the drafter returned: added as a `todo` item "Fix: …" (chat-specs.md section 13.5).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FixItem {
    pub id: String,
    pub title: String,
    pub kind: &'static str,
    pub spec_refs: Vec<String>,
    pub remaining: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_by: Option<String>,
}

impl FixItem {
    /// A fix with `title` as given (`Fix: ` is put in front when it lacks it), cut to the store's 300 characters.
    pub fn new(id: impl Into<String>, title: &str, remaining: &str) -> Self {
        let title = title.trim();
        let title = if title.to_lowercase().starts_with("fix:") {
            title.to_string()
        } else {
            format!("Fix: {title}")
        };
        Self {
            id: id.into(),
            title: crate::text::truncate(&title, 300),
            kind: "fix",
            spec_refs: Vec::new(),
            remaining: crate::text::truncate(remaining.trim(), 4000),
            requested_by: None,
        }
    }
}

/// Which headers `upsert_from_specs` sends, and what it knows about them.
#[derive(Debug, Clone, Copy)]
pub struct SpecsUpsert<'a> {
    pub commit: &'a str,
    /// The commit the store took headers from last (its `specs_commit`).
    pub previous: Option<&'a str>,
    pub items: &'a [SpecItem],
    /// Ids an approved draft added: they start `todo`.
    pub approved: &'a [String],
    /// Who asked for the plan each approved id came from.
    pub asked_by: &'a BTreeMap<String, String>,
}

impl Client {
    /// `draft_claim_next { agent, engines? }`: takes the first draft waiting for a computer, or `None`.
    /// `engines` is what this computer's drafter would start on, so the store skips it while limited.
    pub async fn draft_claim_next(
        &self,
        repo_id: u64,
        token: Option<&str>,
        agent: &ClaimNextAgent,
        engines: &[String],
    ) -> Result<Option<DraftClaim>, BacklogError> {
        let mut args = json!({ "agent": agent });
        if !engines.is_empty() {
            args["engines"] = json!(engines);
        }
        let (status, body) = self.send(repo_id, token, "draft_claim_next", args).await?;
        match body.value {
            None | Some(Value::Null) => Ok(None),
            // Another computer with more of its Claude week left takes the plan; ask again next pass.
            Some(Value::Object(o)) if o.contains_key("passed") => {
                if let Some(p) = super::week::Passed::of(&o) {
                    tracing::info!(
                        "left the next plan for {}, which has more of its Claude week left",
                        p.to
                    );
                }
                Ok(None)
            }
            Some(value) => serde_json::from_value(value)
                .map(Some)
                .map_err(|e| BacklogError::BadAnswer {
                    status,
                    code: None,
                    message: format!("couldn't read the draft it claimed: {e}"),
                }),
        }
    }

    /// `draft_report { id, token, ...fields }`: the holder's change to its draft.
    pub async fn draft_report(
        &self,
        repo_id: u64,
        token: Option<&str>,
        id: &str,
        lease: &str,
        report: &DraftReport,
    ) -> Result<(), BacklogError> {
        self.draft_report_decided(repo_id, token, id, lease, report)
            .await
            .map(|_| ())
    }

    /// [`Self::draft_report`], with the store's decision when the report carried a plan's facts.
    pub async fn draft_report_decided(
        &self,
        repo_id: u64,
        token: Option<&str>,
        id: &str,
        lease: &str,
        report: &DraftReport,
    ) -> Result<Option<PlanDecision>, BacklogError> {
        let mut args = serde_json::to_value(report).unwrap_or_else(|_| json!({}));
        args["id"] = json!(id);
        args["token"] = json!(lease);
        // The store keeps a `designNote` of at most DESIGN_NOTE_MAX (500) characters and refuses a longer one.
        if let Some(note) = args.get("designNote").and_then(Value::as_str) {
            args["designNote"] = json!(crate::text::truncate(note, DESIGN_NOTE_MAX));
        }
        let (_, answer) = self.send(repo_id, token, "draft_report", args).await?;
        Ok(report
            .plan
            .as_ref()
            .and_then(|_| PlanDecision::of(answer.value.as_ref())))
    }

    /// `draft_skip_design { id }`: clears a plan's `needsDesign` and `designNote`, held or not (backlog/06.2). No lease
    /// goes with it, so it works whoever holds the plan; the store refuses a finished one.
    pub async fn draft_skip_design(&self, repo_id: u64, token: Option<&str>, id: &str) -> Result<(), BacklogError> {
        self.send(repo_id, token, "draft_skip_design", json!({ "id": id }))
            .await
            .map(|_| ())
    }

    /// `draft_take { id, agent }`: takes a draft nobody holds (or this computer's own) to land or close it.
    pub async fn draft_take(
        &self,
        repo_id: u64,
        token: Option<&str>,
        id: &str,
        agent: &ClaimNextAgent,
    ) -> Result<TakenDraft, BacklogError> {
        let args = json!({ "id": id, "agent": agent });
        self.op(repo_id, token, "draft_take", args, "taken draft").await
    }

    /// `draft_release { id, token }`: lets go of a draft taken but not acted on.
    pub async fn draft_release(
        &self,
        repo_id: u64,
        token: Option<&str>,
        id: &str,
        lease: &str,
    ) -> Result<(), BacklogError> {
        let args = json!({ "id": id, "token": lease });
        self.send(repo_id, token, "draft_release", args).await.map(|_| ())
    }

    /// `draft_answer_taken { id, token }`: the holder took the person's answer, so the store clears it.
    pub async fn draft_answer_taken(
        &self,
        repo_id: u64,
        token: Option<&str>,
        id: &str,
        lease: &str,
    ) -> Result<(), BacklogError> {
        let args = json!({ "id": id, "token": lease });
        self.send(repo_id, token, "draft_answer_taken", args).await.map(|_| ())
    }

    /// `draft_front { id }`: a do-next press moves a draft waiting for a computer to the front (`conflict`
    /// when it isn't waiting).
    pub async fn draft_front(&self, repo_id: u64, token: Option<&str>, id: &str) -> Result<(), BacklogError> {
        self.send(repo_id, token, "draft_front", json!({ "id": id }))
            .await
            .map(|_| ())
    }

    /// `upsert_from_specs`: the parsed headers at a commit; answers the ids it added.
    pub async fn upsert_from_specs(
        &self,
        repo_id: u64,
        token: Option<&str>,
        upsert: &SpecsUpsert<'_>,
    ) -> Result<Vec<String>, BacklogError> {
        #[derive(Deserialize)]
        struct Added {
            added: Vec<String>,
        }
        let args = json!({
            "commit": upsert.commit,
            "previous": upsert.previous,
            "items": upsert.items,
            "approved": upsert.approved,
            "askedBy": upsert.asked_by,
        });
        let answer: Added = self
            .op(repo_id, token, "upsert_from_specs", args, "upsert answer")
            .await?;
        Ok(answer.added)
    }

    /// `add { via: "chat", items: [fix] }`: a `todo` item "Fix: …". An id already taken is a `conflict`.
    pub async fn add_fix(&self, repo_id: u64, token: Option<&str>, fix: &FixItem) -> Result<(), BacklogError> {
        self.send(repo_id, token, "add", json!({ "via": "chat", "items": [fix] }))
            .await
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod plan_decision_tests {
    use super::*;

    #[test]
    fn the_stores_decision_is_read_and_no_answer_is_no_decision() {
        let said = |v: Value| PlanDecision::of(Some(&v));
        assert_eq!(
            said(json!({ "approve": true, "needsDesign": false })),
            Some(PlanDecision {
                approve: true,
                ..Default::default()
            })
        );
        let full = said(
            json!({ "approve": false, "needsDesign": false, "table": true, "tier": "tryfirst", "reasons": ["a feature", 3, "a screen"] }),
        );
        let full = full.unwrap();
        assert!(full.table);
        assert_eq!(
            (full.tier.as_deref(), full.reasons),
            (Some("tryfirst"), vec!["a feature".to_string(), "a screen".to_string()])
        );
        // No tier back, or one that is no tier: the careful one, with a reason that says so.
        for none in [json!({ "approve": true }), json!({ "approve": true, "tier": "huge" })] {
            let (tier, why) = said(none).unwrap().tier_or_careful_owned();
            assert_eq!(tier, "needsyou");
            assert!(why[0].contains("could not say"), "{why:?}");
        }
        assert_eq!(said(json!({ "approve": false })), Some(PlanDecision::default()));
        assert_eq!(
            said(json!({ "approve": false, "needsDesign": true })).map(|d| d.needs_design),
            Some(true)
        );
        for none in [
            json!(null),
            json!({}),
            json!({ "approve": "yes" }),
            json!({ "needsDesign": true }),
        ] {
            assert_eq!(
                said(none),
                None,
                "an answer that is no decision leaves the plan for a person"
            );
        }
        assert_eq!(PlanDecision::of(None), None);
    }
}
