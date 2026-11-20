//! Every end the agent can send for each kind of run, checked against the store's rules ([`super::check_args`]).
//! The run tests (build, look, plan, watch, merge) check each end they produce through the same copy; these cover
//! the ends that need no lane or session to make.
use anyhow::anyhow;
use chrono::{TimeZone, Utc};
use serde_json::{Value, json};
use std::time::Duration;

use super::*;
use crate::implementer::publish::Published;
use crate::implementer::{CursorTimeKilled, ImplementerResult, Interrupted, LimitHit, MaxTurns, SilenceKilled};
use crate::runs::chore::{ChoreKind, chore_end};
use crate::runs::{Pushed, end_from_answer, person_steps_end};

const KINDS: [RunKind; 6] = [
    RunKind::Plan,
    RunKind::Build,
    RunKind::Fix,
    RunKind::Watch,
    RunKind::Merge,
    RunKind::Look,
];

fn refused(kind: RunKind, outcome: &RunOutcome, result: Value) -> Result<(), String> {
    check_args(kind, &run_done_args("tok", outcome, result))
}

fn answer(v: Value) -> ImplementerResult {
    serde_json::from_value(v).unwrap()
}

fn pushed() -> Pushed {
    Pushed {
        branch: "retaskable/item-piece-2".into(),
        change: Published {
            url: "https://github.com/o/r/pull/7".into(),
            number: 7,
            created: true,
        },
    }
}

fn questions(text: &str) -> Value {
    json!([{ "question": text, "options": ["a"] }])
}

/// Each status the implementer can answer a build or fix with, every error a session can end in, with and without
/// a pushed change, and over-long words: all of them pass for a build and for a fix.
#[test]
fn every_build_and_fix_end_is_one_the_store_takes() {
    let long = "x".repeat(5000); // long words are cut, not sent whole
    let answers = [
        json!({ "status": "pr_opened", "summary": "s" }),
        json!({ "status": "needs_clarification", "summary": "s", "questions": questions("Which?") }),
        json!({ "status": "needs_clarification", "summary": "s", "questions": questions(&long) }),
        json!({ "status": "needs_clarification", "summary": "s", "questions": [] }),
        json!({ "status": "person_steps", "summary": "s", "personSteps": ["Add the secret.", "Set  the\nvalue."] }),
        json!({ "status": "person_steps", "summary": "s" }),
        json!({ "status": "nothing_left", "summary": "s" }),
        json!({ "status": "failed", "summary": long }),
        json!({ "status": "waits_on", "summary": long, "waitsOn": ["a", "b"] }),
        json!({ "status": "waits_on", "summary": "s" }),
        json!({ "status": "something_new", "summary": long }),
    ];
    let errors = [
        anyhow!(Interrupted("stopped".into())),
        anyhow!(LimitHit {
            why: "limit".into(),
            resets_in: Some(Duration::from_secs(3600)),
        }),
        anyhow!(LimitHit {
            why: "limit".into(),
            resets_in: None,
        }),
        anyhow!(SilenceKilled(long.clone())),
        anyhow!(CursorTimeKilled(long.clone())),
        anyhow!(MaxTurns(long.clone())),
        anyhow!(long.clone()),
    ];
    let now = Utc.with_ymd_and_hms(2026, 11, 16, 12, 0, 0).unwrap();
    for kind in [RunKind::Build, RunKind::Fix] {
        for pushed in [None, Some(pushed())] {
            let mut ends: Vec<_> = answers
                .iter()
                .map(|a| end_from_answer(Ok(answer(a.clone())), pushed.as_ref(), &[], "claude", now))
                .collect();
            ends.extend(
                errors
                    .iter()
                    .map(|e| end_from_answer(Err(anyhow!("{e:#}")), pushed.as_ref(), &[], "claude", now)),
            );
            ends.push(end_from_answer(
                Err(anyhow!(LimitHit {
                    why: "limit".into(),
                    resets_in: Some(Duration::from_secs(60)),
                })),
                pushed.as_ref(),
                &[],
                "claude",
                now,
            ));
            for (outcome, result) in ends {
                let (outcome, result) = person_steps_end(outcome, result);
                if let Err(why) = refused(kind, &outcome, result.clone()) {
                    panic!("{kind:?} {outcome:?} {result}: {why}");
                }
            }
        }
    }
}

/// A chore never waits on a person: its questions and steps end `failed`, whatever the chore.
#[test]
fn a_chores_ends_are_ones_the_store_takes() {
    let steps = json!({ "steps": ["Add the secret."] });
    let q = json!({ "questions": questions("Which?") });
    for kind in [
        ChoreKind::FixMain,
        ChoreKind::Tidy,
        ChoreKind::Tests,
        ChoreKind::Bugs,
        ChoreKind::Specs,
        ChoreKind::SyncMain,
        ChoreKind::PlaytestFix,
    ] {
        for (outcome, result) in [
            (
                RunOutcome::Questions,
                json!({ "what": "Which?", "questions": [{ "question": "Which?", "options": [{ "label": "a" }] }] }),
            ),
            (RunOutcome::PersonSteps, steps.clone()),
            (
                RunOutcome::PrOpened,
                json!({ "branch": "b", "prUrl": "https://x/pull/1", "prNumber": 1 }),
            ),
            (RunOutcome::NothingLeft, json!({})),
        ] {
            let end = chore_end(
                kind,
                outcome.clone(),
                if outcome == RunOutcome::PersonSteps { &steps } else { &q },
            );
            let (end, result) = person_steps_end(end, result);
            assert_eq!(refused(RunKind::Build, &end, result), Ok(()), "{kind:?} {outcome:?}");
        }
    }
}

/// Every kind ends `interrupted` and `failed` (with a long why), and `limited` where the kind has it.
#[test]
fn interrupted_failed_and_limited_ends_fit_every_kind() {
    let until = Utc.with_ymd_and_hms(2026, 11, 16, 13, 0, 0).single();
    for kind in KINDS {
        assert_eq!(refused(kind, &RunOutcome::Interrupted, json!({})), Ok(()), "{kind:?}");
        assert_eq!(
            refused(kind, &RunOutcome::Failed("y".repeat(900)), json!({ "outside": true })),
            Ok(()),
            "{kind:?}"
        );
        let limited = RunOutcome::Limited {
            engine: "codex".into(),
            until,
        };
        let got = refused(kind, &limited, json!({}));
        let has = matches!(kind, RunKind::Plan | RunKind::Build | RunKind::Fix | RunKind::Look);
        assert_eq!(got.is_ok(), has, "{kind:?}: {got:?}");
    }
}

/// The ends the pure parts make for the other kinds.
#[test]
fn watch_merge_plan_and_look_ends_fit() {
    let ok = |kind, outcome: RunOutcome, result: Value| {
        assert_eq!(
            refused(kind, &outcome, result.clone()),
            Ok(()),
            "{kind:?} {outcome:?} {result}"
        )
    };
    ok(RunKind::Watch, RunOutcome::Green, json!({ "note": "no CI" }));
    ok(
        RunKind::Merge,
        RunOutcome::Merged,
        json!({ "commit": "0123456789abcdef0123456789abcdef01234567" }),
    );
    ok(RunKind::Merge, RunOutcome::Updated, json!({ "head": "0123456" }));
    ok(
        RunKind::Merge,
        RunOutcome::Conflict,
        json!({ "instruction": "merge the base in", "files": ["a"] }),
    );
    let piece = json!({ "title": "A", "what": "a", "doneWhen": "d", "needs": [] });
    ok(RunKind::Plan, RunOutcome::Planned, json!({ "pieces": [piece] }));
    for decision in ["again", "start_over", "wait", "person"] {
        ok(
            RunKind::Look,
            RunOutcome::Looked,
            json!({ "decision": decision, "reason": "why", "engine": "claude" }),
        );
    }
}

/// The checker itself: the shapes that were sent before, and each rule it copies, are refused.
#[test]
fn the_copy_of_the_stores_rules_refuses_what_the_store_refuses() {
    let bad = |kind, outcome: RunOutcome, result: Value| {
        assert!(
            refused(kind, &outcome, result.clone()).is_err(),
            "{kind:?} {outcome:?} {result}"
        )
    };
    // 2026-11-16: `change: { number, url }` instead of `prUrl` and `prNumber`.
    bad(
        RunKind::Build,
        RunOutcome::PrOpened,
        json!({ "branch": "b", "change": { "number": 7, "url": "u" } }),
    );
    bad(
        RunKind::Build,
        RunOutcome::PrOpened,
        json!({ "branch": "b", "prUrl": "u", "prNumber": "7" }),
    );
    bad(
        RunKind::Build,
        RunOutcome::Questions,
        json!({ "questions": questions("Which?") }),
    );
    bad(RunKind::Build, RunOutcome::PersonSteps, json!({ "steps": ["a"] }));
    bad(RunKind::Watch, RunOutcome::Red, json!({ "checks": [] }));
    bad(RunKind::Watch, RunOutcome::PrOpened, json!({}));
    bad(RunKind::Merge, RunOutcome::Merged, json!({ "commit": "abc123" }));
    bad(RunKind::Plan, RunOutcome::Planned, json!({ "pieces": [] }));
    bad(
        RunKind::Look,
        RunOutcome::Looked,
        json!({ "decision": "maybe", "reason": "r" }),
    );
    // `until` is epoch milliseconds, never a date string.
    let args = json!({ "token": "t", "outcome": "limited", "result": { "engine": "claude", "until": "2026-01-01T01:00:00Z" } });
    assert!(check_args(RunKind::Build, &args).is_err());
    // A failed why past 300 characters is cut by `run_done_args`, not sent whole.
    let args = run_done_args("t", &RunOutcome::Failed("z".repeat(1000)), json!({}));
    assert_eq!(args["result"]["why"].as_str().unwrap().chars().count(), 300);
}
