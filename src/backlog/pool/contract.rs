//! A Rust copy of the store's argument rules for the pool ops (test only): what the agent sends must pass what the store
//! checks, or the store answers 400 and the report or the end is lost. Copied from Retaskable.com's
//! `shared/backlogPoolOps.ts` (`health`, `task_end`, `usageOf`, `behind_main`), `shared/backlogChoreTriggerOps.ts`
//! (`red_main`, `TRIGGERS`), `shared/backlogFixes.ts` (`staging_smoke`), `shared/backlogRun.ts` (`str`,
//! `optStr`, `strs`, `bool`) and `shared/jobs.ts` (`CAN`), and `shared/backlogOps.ts` (`claim`, `chores_get`, `chore_nofix`), at origin/main on 2026-11-17. Change them there first, then here.
use serde_json::{Map, Value};

type Check = Result<(), String>;

fn len(s: &str) -> usize {
    s.encode_utf16().count()
}

fn obj<'a>(v: &'a Value, what: &str) -> Result<&'a Map<String, Value>, String> {
    v.as_object().ok_or_else(|| format!("{what} must be an object"))
}

/// `str(args, key, max)`: a non-blank string of at most `max`.
fn req_str(o: &Map<String, Value>, key: &str, max: usize) -> Check {
    match o.get(key).and_then(Value::as_str) {
        Some(s) if !s.trim().is_empty() && len(s) <= max => Ok(()),
        _ => Err(format!("{key} is missing or too long")),
    }
}

/// `optStr`: absent, null, or a string of at most `max`.
fn opt_str(o: &Map<String, Value>, key: &str, max: usize) -> Check {
    match o.get(key) {
        None | Some(Value::Null) => Ok(()),
        Some(Value::String(s)) if len(s) <= max => Ok(()),
        _ => Err(format!("{key} isn't text, or is too long")),
    }
}

fn req_bool(o: &Map<String, Value>, key: &str) -> Check {
    o.get(key)
        .and_then(Value::as_bool)
        .map(|_| ())
        .ok_or_else(|| format!("{key} must be true or false"))
}

/// `num(v, what, min, max, int)`.
fn num(v: &Value, what: &str, min: f64, max: f64, int: bool) -> Check {
    match v.as_f64() {
        Some(n) if n >= min && n <= max && (!int || n.fract() == 0.0) => Ok(()),
        _ => Err(format!("{what} must be a number from {min} to {max}")),
    }
}

/// `health` (backlogPoolOps.ts).
pub fn health(args: &Value) -> Check {
    let a = obj(args, "args")?;
    for r in a
        .get("running")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(40)
    {
        let r = obj(r, "running")?;
        req_str(r, "id", 100)?;
        // A chore that stopped (backlogPoolOps.ts: `state` is `needs_human` or absent, `why` at most 1000).
        match r.get("state") {
            None | Some(Value::Null) => {}
            Some(Value::String(s)) if s == "needs_human" => {}
            _ => return Err("running state must be needs_human".into()),
        }
        opt_str(r, "why", 1000)?;
    }
    req_bool(a, "paused")?;
    req_bool(a, "draining")?;
    if let Some(f) = a.get("ownFailure").filter(|f| !f.is_null()) {
        let f = obj(f, "ownFailure")?;
        num(f.get("at").unwrap_or(&Value::Null), "at", 0.0, 1e15, true)?;
        opt_str(f, "why", 300)?;
    }
    opt_str(a, "refused", 200)?;
    if let Some(can) = a.get("can")
        && !can.is_array()
    {
        return Err("can must be a list".into());
    }
    if let Some(agent) = a.get("agent") {
        opt_str(obj(agent, "agent")?, "machine", 200).ok();
    }
    if let Some(u) = a.get("usage") {
        let u = obj(u, "usage")?;
        opt_str(u, "status", 40)?;
        for k in ["utilization", "weekUtilization"] {
            if let Some(v) = u.get(k).filter(|v| !v.is_null()) {
                num(v, k, 0.0, 1.0, false)?;
            }
        }
        for k in ["resetsAt", "weekResetsAt"] {
            if let Some(v) = u.get(k).filter(|v| !v.is_null()) {
                num(v, k, 0.0, 1e12, false)?;
            }
        }
    }
    if let Some(spend) = a.get("spend") {
        for v in obj(spend, "spend")?.values() {
            num(v, "spend", 0.0, 1e9, false)?;
        }
    }
    if let Some(limits) = a.get("limits") {
        for v in obj(limits, "limits")?.values() {
            num(v, "limits", 0.0, 1e15, true)?;
        }
    }
    if let Some(main) = a.get("main").filter(|m| !m.is_null()) {
        main_facts(main)?;
    }
    opt_str(a, "project", 100)
}

fn text(o: &Map<String, Value>, key: &str, max: usize) -> Check {
    match o.get(key).and_then(Value::as_str) {
        Some(s) if !s.is_empty() && len(s) <= max => Ok(()),
        _ => Err(format!("{key} must be a short text")),
    }
}

/// `sha` of `mainLook.ts`: 7 to 64 hex digits.
fn sha(o: &Map<String, Value>, key: &str) -> Check {
    match o.get(key).and_then(Value::as_str) {
        Some(s)
            if (7..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) =>
        {
            Ok(())
        }
        _ => Err(format!("{key} must be a commit")),
    }
}

/// `names`: absent or a list of non-empty names of at most 300 characters; at most 50 are kept, so more is no refusal.
fn names(o: &Map<String, Value>, key: &str) -> Check {
    match o.get(key) {
        None => Ok(()),
        Some(Value::Array(list)) => {
            for n in list.iter().take(50) {
                match n.as_str() {
                    Some(n) if !n.is_empty() && len(n) <= 300 => {}
                    _ => return Err(format!("{key} holds a name that doesn't fit")),
                }
            }
            Ok(())
        }
        Some(_) => Err(format!("{key} must be a list")),
    }
}

fn count(v: Option<&Value>, what: &str) -> Check {
    match v.and_then(Value::as_u64) {
        Some(n) if n <= 1_000_000 => Ok(()),
        _ => Err(format!("{what} must be a whole number")),
    }
}

/// `cleanMainFacts` (shared/mainLook.ts): `health`'s `main`. One that does not fit fails the whole `health`.
pub fn main_facts(v: &Value) -> Check {
    let o = obj(v, "main")?;
    text(o, "branch", 200)?;
    sha(o, "sha")?;
    match o.get("tree") {
        None | Some(Value::Null) => {}
        Some(Value::String(t)) if !t.is_empty() && len(t) <= 100 => {}
        _ => return Err("tree must be a short text".into()),
    }
    if let Some(b) = o.get("behind") {
        let b = obj(b, "behind")?;
        text(b, "branch", 200)?;
        sha(b, "head")?;
        count(b.get("count"), "behind.count")?;
    }
    if let Some(b) = o.get("baseline") {
        let b = obj(b, "baseline")?;
        sha(b, "sha")?;
        names(b, "failed")?;
    }
    if let Some(c) = o.get("ci") {
        let c = obj(c, "ci")?;
        sha(c, "sha")?;
        if !matches!(
            c.get("state").and_then(Value::as_str),
            Some("pass" | "pending" | "fail" | "none")
        ) {
            return Err("ci.state must be pass, pending, fail or none".into());
        }
        names(c, "failed")?;
    }
    if let Some(c) = o.get("coverage") {
        let c = obj(c, "coverage")?;
        text(c, "base", 200)?;
        match c.get("tree") {
            Some(Value::Null) => {}
            Some(Value::String(t)) if !t.is_empty() && len(t) <= 100 => {}
            _ => return Err("coverage.tree must be a short text or null".into()),
        }
    }
    Ok(())
}

/// `cleanMainResult` (shared/mainLook.ts): what a `main` job's `task_end` result carries.
pub fn main_result(v: &Value) -> Check {
    let o = obj(v, "result")?;
    sha(o, "sha")?;
    if let Some(b) = o.get("baseline") {
        let b = obj(b, "baseline")?;
        sha(b, "sha")?;
        names(b, "failed")?;
    }
    if let Some(c) = o.get("ci") {
        let c = obj(c, "ci")?;
        sha(c, "sha")?;
        if !matches!(
            c.get("state").and_then(Value::as_str),
            Some("pass" | "pending" | "fail" | "none")
        ) {
            return Err("ci.state must be pass, pending, fail or none".into());
        }
        names(c, "failed")?;
    }
    if let Some(b) = o.get("brief")
        && !b.is_string()
    {
        return Err("brief must be a text".into());
    }
    if let Some(r) = o.get("rebase") {
        let r = obj(r, "rebase")?;
        names(r, "conflict")?;
        opt_str(r, "error", 500)?;
    }
    Ok(())
}

/// `task_end` (backlogPoolOps.ts): the id and `ok`; a limit hit, when sent, names an engine and an `until`.
pub fn task_end(args: &Value) -> Check {
    let a = obj(args, "args")?;
    req_str(a, "id", 100)?;
    req_bool(a, "ok")
}

/// `chore_trigger` (backlogChoreTriggerOps.ts and the triggers it dispatches to).
pub fn chore_trigger(args: &Value) -> Check {
    let a = obj(args, "args")?;
    req_str(a, "trigger", 40)?;
    let data = || obj(a.get("data").unwrap_or(&Value::Null), "data");
    match a["trigger"].as_str().unwrap_or("") {
        "red_main" => {
            let d = data()?;
            req_str(d, "sha", 64)?;
            req_str(d, "branch", 200)?;
            let checks = d.get("checks").and_then(Value::as_array);
            for c in checks.into_iter().flatten().take(50) {
                req_str(obj(c, "check")?, "name", 200)?;
            }
            opt_str(d, "brief", 20_000)
        }
        "staging_smoke" => {
            let d = data()?;
            req_str(d, "sha", 64)?;
            req_str(d, "url", 500)?;
            req_str(d, "command", 1000)?;
            opt_str(d, "tail", 20_000)
        }
        "behind_main" => {
            req_str(a, "branch", 200)?;
            req_str(a, "main", 200)?;
            num(a.get("behind").unwrap_or(&Value::Null), "behind", 1.0, 1e6, true)?;
            opt_str(a, "project", 100)
        }
        other => Err(format!("no trigger called {other}")),
    }
}

/// `chore_nofix` (backlogOps `chore_nofix`): a sha and a branch.
pub fn chore_nofix(args: &Value) -> Check {
    let a = obj(args, "args")?;
    req_str(a, "sha", 64)?;
    req_str(a, "branch", 200)
}

/// `claim` by name (`backlogOps.ts` `claim`): the item's id (`str`), and `agent`, which `record()` takes as absent, null or an
/// object. `because` and `did` are a person's claim (`oneOf` on `did` only on a parked item), which a pool task never sends.
pub fn claim(args: &Value) -> Check {
    let a = obj(args, "args")?;
    req_str(a, "id", 100)?;
    match a.get("agent") {
        None | Some(Value::Null) | Some(Value::Object(_)) => Ok(()),
        _ => Err("Expected an object".into()),
    }
}

/// `chores_get` (`backlogOps.ts`): reads no argument, so any object (or none) passes.
pub fn chores_get(args: &Value) -> Check {
    match args {
        Value::Null | Value::Object(_) => Ok(()),
        _ => Err("args must be an object".into()),
    }
}

/// The store's check of a pool op's args, by name: what `fake_store` runs on every request. An op that isn't one of the
/// pool's is refused, so a test never passes by sending something the store doesn't know.
pub fn check_op(op: &str, args: &Value) -> Check {
    match op {
        "health" => health(args),
        "task_end" => task_end(args),
        "chore_trigger" => chore_trigger(args),
        "chore_nofix" => chore_nofix(args),
        "claim" => claim(args),
        "chores_get" => chores_get(args),
        other => Err(format!("{other} isn't a pool op")),
    }
}

/// A fake store like `backlog::tests::store`, answering every op with `reply`, except that each request is first checked
/// against [`check_op`]: one the store would refuse is answered 400 `bad_request` with the reason (and still recorded), so
/// a test that unwraps the client's answer fails on a wrong argument, as the real store would.
pub(crate) async fn fake_store(reply: Value) -> (String, crate::backlog::tests::Seen) {
    use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
    let seen: crate::backlog::tests::Seen = Default::default();
    let app =
        Router::new()
            .route(
                "/v1/repos/{id}/ops",
                post(
                    move |State((seen, reply)): State<(crate::backlog::tests::Seen, Value)>,
                          Json(body): Json<Value>| async move {
                        seen.lock().unwrap().push(body.clone());
                        let op = body["op"].as_str().unwrap_or("");
                        match check_op(op, body.get("args").unwrap_or(&Value::Null)) {
                            Ok(()) => (StatusCode::OK, Json(reply)),
                            Err(why) => (
                                StatusCode::BAD_REQUEST,
                                Json(serde_json::json!({ "error": "bad_request", "message": why })),
                            ),
                        }
                    },
                ),
            )
            .with_state((seen.clone(), reply));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), seen)
}
