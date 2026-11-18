//! A Rust copy of the store's rules for `run_done` (test only): what the agent sends must pass what the store
//! checks, or the store answers 400 and the end is dropped. Copied from Retaskable.com's `shared/backlogPieceOps.ts`
//! (`OUTCOMES`, `run_done`, `pieceDone`), `shared/pieceLooks.ts` (`lookOf`, `engineOf`), `shared/pieces.ts`
//! (`planInput`, `RUN_ENGINES`, `LOOK_DECISIONS`, `HELD_MAX`) and `shared/backlogRun.ts` (`str`, `optStr`, `whole`,
//! `optTime`, `oneOf`, `SHA`), at origin/main on 2026-11-16. Change them there first, then here.
use serde_json::{Map, Value};

use crate::backlog::runs::{RunKind, RunOutcome, run_done_args};
use crate::orchestrator::driver::RunEnd;

/// `OUTCOMES[kind]` (backlogPieceOps.ts:15-22).
fn outcomes(kind: RunKind) -> &'static [&'static str] {
    match kind {
        RunKind::Plan => &["planned", "failed", "interrupted", "limited"],
        RunKind::Build | RunKind::Fix => &[
            "pr_opened",
            "questions",
            "split",
            "nothing_left",
            "failed",
            "interrupted",
            "limited",
        ],
        RunKind::Watch => &["green", "red", "failed", "interrupted"],
        RunKind::Merge => &["merged", "updated", "conflict", "failed", "interrupted"],
        RunKind::Look => &["looked", "failed", "interrupted", "limited"],
    }
}

const RUN_ENGINES: [&str; 4] = ["claude", "cursor", "pi", "codex"]; // pieces.ts:27
const LOOK_DECISIONS: [&str; 5] = ["again", "start_over", "split", "wait", "person"]; // pieces.ts:35
const HELD_MAX: usize = 500; // pieces.ts:24
const MAX_PIECES: usize = 30; // pieces.ts:22

type Check = Result<(), String>;

fn len(s: &str) -> usize {
    s.encode_utf16().count() // JS string length
}

/// `str(args, key, max)` (backlogRun.ts:790): a non-blank string of at most `max`.
fn req_str<'a>(o: &'a Map<String, Value>, key: &str, max: usize) -> Result<&'a str, String> {
    match o.get(key).and_then(Value::as_str) {
        Some(s) if !s.trim().is_empty() && len(s) <= max => Ok(s),
        _ => Err(format!("{key} is missing or too long")),
    }
}

/// `optStr` (backlogRun.ts:806): absent, null, or a string of at most `max`.
fn opt_str<'a>(o: &'a Map<String, Value>, key: &str, max: usize) -> Result<Option<&'a str>, String> {
    match o.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if len(s) <= max => Ok(Some(s)),
        _ => Err(format!("{key} isn't text, or is too long")),
    }
}

/// `whole` (backlogRun.ts:847): an integer of 0 or more.
fn whole(o: &Map<String, Value>, key: &str) -> Check {
    match o.get(key).and_then(Value::as_u64) {
        Some(_) => Ok(()),
        None => Err(format!("{key} must be a whole number")),
    }
}

fn one_of(o: &Map<String, Value>, key: &str, allowed: &[&str]) -> Check {
    match o.get(key).and_then(Value::as_str) {
        Some(s) if allowed.contains(&s) => Ok(()),
        _ => Err(format!("{key} must be one of {}", allowed.join(", "))),
    }
}

fn sha(s: &str) -> bool {
    (7..=40).contains(&s.len()) && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) // backlogRun.ts:54
}

/// `planInput` (pieces.ts:203): 1 to 30 pieces, each with title, what, doneWhen and `needs` naming earlier ones.
fn plan_input(v: Option<&Value>) -> Check {
    let pieces = match v.and_then(Value::as_array) {
        Some(p) if !p.is_empty() && p.len() <= MAX_PIECES => p,
        _ => return Err(format!("pieces must be a list of 1 to {MAX_PIECES}")),
    };
    for (i, p) in pieces.iter().enumerate() {
        let o = p.as_object().ok_or(format!("Piece {} isn't an object", i + 1))?;
        match o.get("needs") {
            None | Some(Value::Null) => {}
            Some(Value::Array(needs)) => {
                if needs
                    .iter()
                    .any(|n| !n.as_u64().is_some_and(|n| n >= 1 && (n as usize) <= i))
                {
                    return Err(format!("Piece {}: needs must name earlier pieces", i + 1));
                }
            }
            Some(_) => return Err(format!("Piece {}: needs must name earlier pieces", i + 1)),
        }
        for (key, max) in [("title", 200), ("what", 4000), ("doneWhen", 2000)] {
            req_str(o, key, max).map_err(|e| format!("Piece {}: {e}", i + 1))?;
        }
    }
    Ok(())
}

/// The checks `run_done` (backlogPieceOps.ts:358) makes of `args` for a run of `kind`: the outcome is one the kind
/// has, and the result has what that outcome's handler reads.
pub fn check_args(kind: RunKind, args: &Value) -> Check {
    let o = args.as_object().ok_or("args isn't an object")?;
    req_str(o, "token", 100)?;
    let outcome = o.get("outcome").and_then(Value::as_str).unwrap_or_default();
    if !outcomes(kind).contains(&outcome) {
        return Err(format!("A {kind:?} run can't end {outcome:?}"));
    }
    let empty = Map::new();
    let r = match o.get("result") {
        None | Some(Value::Null) => &empty,
        Some(Value::Object(m)) => m,
        Some(_) => return Err("Expected an object".into()),
    };
    match outcome {
        "failed" => req_str(r, "why", 300).map(drop), // backlogPieceOps.ts:362
        "interrupted" | "nothing_left" | "green" | "updated" => Ok(()),
        "limited" => {
            one_of(r, "engine", &RUN_ENGINES)?; // backlogPieceOps.ts:46, optTime for `until`
            match r.get("until") {
                None | Some(Value::Null) => Ok(()),
                Some(v) if v.as_u64().is_some() => Ok(()),
                Some(_) => Err("until isn't a time".into()),
            }
        }
        "looked" => {
            // pieceLooks.ts:9-12 `lookOf`, and `engineOf`.
            opt_str(r, "instructions", 4000)?;
            one_of(r, "decision", &LOOK_DECISIONS)?;
            req_str(r, "reason", 2000)?;
            let decision = r.get("decision").and_then(Value::as_str).unwrap_or_default();
            if !matches!(r.get("engine"), None | Some(Value::Null))
                && ["again", "start_over", "wait"].contains(&decision)
            {
                one_of(r, "engine", &RUN_ENGINES)?;
            }
            if decision == "split" && kind == RunKind::Look && r.get("pieces").is_some() {
                plan_input(r.get("pieces"))?;
            }
            Ok(())
        }
        "planned" => {
            plan_input(r.get("pieces"))?; // backlogPieceOps.ts:326
            opt_str(r, "noFlag", 200).map(drop)
        }
        "split" => plan_input(r.get("pieces")), // backlogPieceOps.ts:172
        "pr_opened" => {
            req_str(r, "branch", 200)?; // backlogPieceOps.ts:176-178
            req_str(r, "prUrl", 500)?;
            whole(r, "prNumber")?;
            if opt_str(r, "approved", 40)?.is_some_and(|a| !a.is_empty() && !sha(a)) {
                return Err("approved isn't a commit".into());
            }
            opt_str(r, "held", HELD_MAX)?;
            r.get("diff").map_or(Ok(()), diff_facts) // backlogPieceOps.ts:193-194 `cleanDiffFacts`
        }
        "questions" => req_str(r, "what", 4000).map(drop), // backlogPieceOps.ts:187
        "red" => req_str(r, "what", 4000).map(drop),       // backlogPieceOps.ts:205
        "merged" => {
            let commit = req_str(r, "commit", 40)?; // backlogPieceOps.ts:209
            if !sha(commit) {
                return Err("commit isn't a commit".into());
            }
            opt_str(r, "version", 20).map(drop)
        }
        "conflict" => opt_str(r, "what", 4000).map(drop), // backlogPieceOps.ts:224 (default arm)
        other => Err(format!("outcome {other} isn't one the store handles")),
    }
}

/// `cleanDiffFacts` (shared/diffFacts.ts, Retaskable.com's store PR D): `{ unreadable: true }`, or `{ files: [{ path, added, removed?, addedText? }],
/// newDependencies?, size? }` with whole numbers, at most 5000 files and a size of S, M, L or XL; anything else is refused.
fn diff_facts(v: &Value) -> Check {
    if v.is_null() {
        return Ok(());
    }
    let Some(d) = v.as_object() else {
        return Err("diff must be an object".into());
    };
    if d.get("unreadable") == Some(&Value::Bool(true)) {
        return Ok(());
    }
    let files = d.get("files").and_then(Value::as_array).ok_or("diff has no files")?;
    if files.len() > 5000 {
        return Err("diff has too many files".into());
    }
    let whole_num = |x: Option<&Value>, or: Option<u64>| -> Result<(), String> {
        match x {
            None => or.map(drop).ok_or_else(|| "a count is missing".to_string()),
            Some(n) if n.as_u64().is_some_and(|n| n <= 1_000_000_000) => Ok(()),
            _ => Err("a count isn't a whole number".into()),
        }
    };
    for f in files {
        let path = f.get("path").and_then(Value::as_str).unwrap_or_default();
        if path.is_empty() || len(path) > 500 {
            return Err("a file has no path".into());
        }
        whole_num(f.get("added"), None)?;
        whole_num(f.get("removed"), Some(0))?;
        if f.get("addedText").is_some_and(|t| !t.is_string()) {
            return Err("addedText isn't text".into());
        }
    }
    if d.get("newDependencies").is_some_and(|b| !b.is_boolean()) {
        return Err("newDependencies isn't true or false".into());
    }
    if d.get("size")
        .is_some_and(|s| !s.as_str().is_some_and(|s| ["S", "M", "L", "XL"].contains(&s)))
    {
        return Err("size isn't S, M, L or XL".into());
    }
    Ok(())
}

/// The checks `settings_report` (`shared/backlogTeamOps.ts:49`, from `applyReport` in `shared/projectSettings.ts`) makes
/// of `args`: `machine` text of 1 to 200, `allowed` a whole number, `denied` and `baseFailed` objects when given,
/// `appliedVersion` a whole number and `migrated` a boolean when given.
pub fn check_settings_report(args: &Value) -> Check {
    let o = args.as_object().ok_or("args isn't an object")?;
    req_str(o, "machine", 200)?;
    whole(o, "allowed")?;
    if let Some(d) = o.get("denied") {
        let d = d.as_object().ok_or("denied isn't an object")?;
        whole(d, "commandsVersion")?;
        match d.get("fields").and_then(Value::as_array) {
            Some(f)
                if f.len() <= 1000
                    && f.iter()
                        .all(|x| x.as_str().is_some_and(|x| !x.trim().is_empty() && len(x) <= 100)) => {}
            _ => return Err("fields must be a list".into()),
        }
    }
    if let Some(b) = o.get("baseFailed") {
        let b = b.as_object().ok_or("baseFailed isn't an object")?;
        req_str(b, "baseBranch", 200)?;
        req_str(b, "reason", 1000)?;
    }
    if o.contains_key("appliedVersion") {
        whole(o, "appliedVersion")?;
    }
    if o.get("migrated").is_some_and(|m| !m.is_boolean()) {
        return Err("migrated must be true or false".into());
    }
    // `args` has no other key the store reads; a wrong spelling would be dropped silently, so refuse it here.
    const KNOWN: [&str; 6] = [
        "machine",
        "allowed",
        "denied",
        "baseFailed",
        "appliedVersion",
        "migrated",
    ];
    match o.keys().find(|k| !KNOWN.contains(&k.as_str())) {
        Some(k) => Err(format!("{k} isn't a field the store reads")),
        None => Ok(()),
    }
}

/// Builds the `run_done` the agent would send for `end`, the way the client does, and checks it for a run of `kind`.
pub fn check(kind: RunKind, end: &RunEnd) {
    check_outcome(kind, &end.outcome, &end.result);
}

pub fn check_outcome(kind: RunKind, outcome: &RunOutcome, result: &Value) {
    let args = run_done_args("tok", outcome, result.clone());
    if let Err(why) = check_args(kind, &args) {
        panic!("the store would refuse this {kind:?} end with 400 ({why}): {args}");
    }
}

#[cfg(test)]
#[path = "store_contract_tests.rs"]
mod tests;
