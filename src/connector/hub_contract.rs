//! A Rust copy of what the hub accepts from an agent (test only): what the agent sends must pass what Retaskable.com's
//! `shared/cloud.ts` checks (`agentMessage`: `hello`, `snapshot`, `patch`, `session_events`, `alive`, `ack`, `nack`,
//! `pending`, `chat_report`, `bug_report`, `suggestions_ask`, `suggestion_verdict`, `timings_report`), with `shared/stageTimings.ts` (`timingRow`), `shared/cloudSnapshot.ts`
//! (`SESSION_HISTORY_MAX`, `IDEAS_CAPS`), `shared/computerTags.ts` (`TAG`, `MAX_TAGS`) and `shared/bugReports.ts`,
//! `shared/suggestions.ts` (the caps and verdicts), at origin/main on 2026-11-17. Change them there first, then here.
//!
//! The hub parses with zod, whose `z.object` drops a key it doesn't list and whose `z.looseObject` passes one on. A
//! key dropped is a field the site never sees, which is how a wrong name goes unnoticed, so [`check`] also fails on
//! a key the hub would drop, unless [`HUB_DROPS`] names it. A message that fails the schema is dropped whole by the hub.
use serde_json::{Map, Value};

type Check = Result<(), String>;

/// Keys the hub drops that this agent still sends (named so each is a decision, not a surprise). The TypeScript agent
/// no longer sends any of them: `hello.settings` was the retired `agent_settings` channel's, `state.trial` was
/// first-run-check.md section 3.3's report and `state.smoke` is a store fact now. Nothing is dropped any more.
pub const HUB_DROPS: &[&str] = &[];

fn object<'a>(v: &'a Value, what: &str) -> Result<&'a Map<String, Value>, String> {
    v.as_object().ok_or_else(|| format!("{what} must be an object"))
}

fn js_len(s: &str) -> usize {
    s.encode_utf16().count()
}

fn text(v: &Value, what: &str, min: usize, max: usize) -> Check {
    match v.as_str() {
        Some(s) if (min..=max).contains(&js_len(s)) => Ok(()),
        _ => Err(format!("{what} must be text of {min} to {max} characters")),
    }
}

/// An absent key passes; a null one doesn't (zod's `.optional()` takes undefined only).
fn optional(o: &Map<String, Value>, key: &str, f: impl Fn(&Value) -> Check) -> Check {
    o.get(key).map_or(Ok(()), |v| f(v).map_err(|e| format!("{key}: {e}")))
}

fn required<'a>(o: &'a Map<String, Value>, key: &str) -> Result<&'a Value, String> {
    o.get(key).ok_or_else(|| format!("{key} is missing"))
}

fn int(v: &Value, what: &str, min: i64) -> Check {
    match v.as_i64().or_else(|| v.as_u64().map(|u| u as i64)) {
        Some(n) if n >= min => Ok(()),
        _ => Err(format!("{what} must be a whole number of at least {min}")),
    }
}

fn number(v: &Value, what: &str, min: f64) -> Check {
    match v.as_f64() {
        Some(n) if n >= min => Ok(()),
        _ => Err(format!("{what} must be a number of at least {min}")),
    }
}

fn boolean(v: &Value, what: &str) -> Check {
    v.is_boolean()
        .then_some(())
        .ok_or_else(|| format!("{what} must be true or false"))
}

fn one_of(v: &Value, what: &str, allowed: &[&str]) -> Check {
    match v.as_str() {
        Some(s) if allowed.contains(&s) => Ok(()),
        _ => Err(format!("{what} must be one of {}", allowed.join(", "))),
    }
}

fn web_url(v: &Value, what: &str) -> Check {
    match v.as_str() {
        Some(s) if js_len(s) <= 500 && (s.starts_with("http://") || s.starts_with("https://")) => Ok(()),
        _ => Err(format!("{what} must be a web address of at most 500 characters")),
    }
}

/// Keys of `o` outside `known`, which the hub's `z.object` drops (`HUB_DROPS` aside); `scope` names the object.
fn unknown_keys(o: &Map<String, Value>, known: &[&str], scope: &str) -> Check {
    for key in o.keys() {
        if !known.contains(&key.as_str()) && !HUB_DROPS.contains(&format!("{scope}.{key}").as_str()) {
            return Err(format!("{scope}.{key} isn't in the hub's schema, so the hub drops it"));
        }
    }
    Ok(())
}

fn tag(s: &str) -> bool {
    (1..=20).contains(&s.len()) && s.bytes().all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'-'))
}

fn setup_check(v: &Value, what: &str) -> Check {
    let o = object(v, what)?;
    unknown_keys(o, &["ok", "detail", "fix"], what)?;
    match o.get("ok") {
        Some(Value::Bool(_) | Value::Null) => {}
        _ => return Err(format!("{what}.ok must be true, false or null")),
    }
    text(required(o, "detail")?, &format!("{what}.detail"), 0, 300)?;
    optional(o, "fix", |v| text(v, "fix", 0, 300))
}

fn hello(o: &Map<String, Value>) -> Check {
    unknown_keys(
        o,
        &[
            "type",
            "agentVersion",
            "os",
            "machine",
            "capabilities",
            "role",
            "policy",
            "project",
            "checks",
            "setup",
            "ci",
            "computer",
            "tags",
        ],
        "hello",
    )?;
    text(required(o, "agentVersion")?, "agentVersion", 0, 50)?;
    text(required(o, "os")?, "os", 0, 50)?;
    text(required(o, "machine")?, "machine", 0, 200)?;
    let caps = required(o, "capabilities")?
        .as_array()
        .ok_or("capabilities must be a list")?;
    if caps.len() > 50 {
        return Err("capabilities has more than 50".into());
    }
    for c in caps {
        text(c, "a capability", 0, 50)?;
    }
    optional(o, "role", |v| one_of(v, "role", &["chat"]))?;
    let policy = object(required(o, "policy")?, "policy")?;
    unknown_keys(
        policy,
        &[
            "enabled",
            "allowMerges",
            "publishGalleries",
            "requests",
            "sites",
            "promoteFromSite",
            "testflight",
            "testflightAnyMember",
            "autoRollout",
            "releases",
        ],
        "policy",
    )?;
    optional(policy, "sites", |v| {
        let sites = v.as_array().ok_or("sites must be a list")?;
        if sites.len() > 10 {
            return Err("more than 10 sites".into());
        }
        for s in sites {
            let s = object(s, "a site")?;
            unknown_keys(s, &["host", "viewOnly"], "site")?;
            text(required(s, "host")?, "host", 0, 200)?;
            boolean(required(s, "viewOnly")?, "viewOnly")?;
        }
        Ok(())
    })?;
    for key in [
        "enabled",
        "allowMerges",
        "publishGalleries",
        "requests",
        "promoteFromSite",
        "testflight",
        "testflightAnyMember",
        "autoRollout",
        "releases",
    ] {
        optional(policy, key, |v| boolean(v, key))?;
    }
    let project = required(o, "project")?;
    if !project.is_null() {
        let p = object(project, "project")?;
        unknown_keys(
            p,
            &[
                "githubRepo",
                "backlogPath",
                "organisationId",
                "codeHost",
                "codeHostRef",
                "codeHostWas",
                "name",
            ],
            "project",
        )?;
        text(required(p, "githubRepo")?, "project.githubRepo", 0, 200)?;
        if o.get("role").and_then(Value::as_str) != Some("chat") && !p.contains_key("backlogPath") {
            return Err("project.backlogPath is required".into());
        }
        optional(p, "backlogPath", |v| text(v, "backlogPath", 0, 500))?;
        optional(p, "organisationId", |v| int(v, "organisationId", 1))?;
        optional(p, "codeHost", |v| text(v, "codeHost", 0, 40))?;
        optional(p, "codeHostRef", |v| text(v, "codeHostRef", 0, 200))?;
        optional(p, "codeHostWas", |v| text(v, "codeHostWas", 0, 200))?;
        optional(p, "name", |v| text(v, "name", 0, 100))?;
    }
    optional(o, "checks", |v| {
        let c = object(v, "checks")?;
        for key in ["node", "git", "gh", "claude", "claudeSignedIn", "cursor"] {
            setup_check(required(c, key)?, key)?;
        }
        optional(c, "ghInstalled", |v| setup_check(v, "ghInstalled"))
    })?;
    // `ci`'s sealing key: strings, not dropped for being wrong (z.object, no catch).
    optional(o, "ci", |v| {
        let ci = object(v, "ci")?;
        optional(ci, "key", |k| {
            let k = object(k, "ci.key")?;
            for (key, max) in [("kty", 10), ("crv", 10), ("x", 100), ("y", 100)] {
                text(required(k, key)?, key, 0, max)?;
            }
            Ok(())
        })
    })?;
    // `computer`, `setup` and `tags` are `.catch(undefined)`: a bad one is left out, not the hello, but a bad one is still a bug.
    optional(o, "computer", |v| {
        let c = object(v, "computer")?;
        int(required(c, "lanes")?, "computer.lanes", i64::MIN)?;
        let secrets = required(c, "secrets")?.as_array().ok_or("secrets must be a list")?;
        if secrets.len() > 60 {
            return Err("computer.secrets has more than 60".into());
        }
        for s in secrets {
            let s = object(s, "a secret")?;
            text(required(s, "key")?, "secret.key", 0, 300)?;
            text(required(s, "state")?, "secret.state", 0, 20)?;
        }
        Ok(())
    })?;
    // `setup` is `.catch(undefined)` too: a running or last install or sign-in, and the OS it found.
    optional(o, "setup", |v| {
        let s = object(v, "setup")?;
        unknown_keys(s, &["running", "last", "os"], "setup")?;
        let tool = |v: &Value| one_of(v, "tool", &["git", "gh", "claude"]);
        let action = |v: &Value| one_of(v, "action", &["install", "sign_in"]);
        optional(s, "running", |v| {
            let r = object(v, "running")?;
            unknown_keys(r, &["tool", "action", "since", "note"], "setup.running")?;
            tool(required(r, "tool")?)?;
            action(required(r, "action")?)?;
            text(required(r, "since")?, "since", 0, 40)?;
            optional(r, "note", |v| text(v, "note", 0, 500))
        })?;
        optional(s, "last", |v| {
            let l = object(v, "last")?;
            unknown_keys(l, &["tool", "action", "ok", "detail", "at"], "setup.last")?;
            tool(required(l, "tool")?)?;
            action(required(l, "action")?)?;
            boolean(required(l, "ok")?, "ok")?;
            text(required(l, "detail")?, "detail", 0, 300)?;
            text(required(l, "at")?, "at", 0, 40)
        })?;
        optional(s, "os", |v| {
            let o = object(v, "os")?;
            unknown_keys(o, &["id", "like"], "setup.os")?;
            text(required(o, "id")?, "id", 0, 50)?;
            let like = required(o, "like")?.as_array().ok_or("like must be a list")?;
            if like.len() > 10 {
                return Err("more than 10 like".into());
            }
            like.iter().try_for_each(|l| text(l, "like", 0, 50))
        })
    })?;
    optional(o, "tags", |v| {
        let tags = v.as_array().ok_or("tags must be a list")?;
        if tags.len() > 20 {
            return Err("more than 20 tags".into());
        }
        match tags.iter().all(|t| t.as_str().is_some_and(tag)) {
            true => Ok(()),
            false => Err("a tag isn't lowercase letters, digits and dashes of 1 to 20".into()),
        }
    })
}

fn id(v: &Value, what: &str) -> Check {
    text(v, what, 1, 200)
}

fn record(v: &Value, what: &str) -> Result<Map<String, Value>, String> {
    object(v, what).cloned()
}

fn session_history(v: &Value) -> Check {
    let list = v.as_array().ok_or("sessionHistory must be a list")?;
    if list.len() > 50 {
        return Err("sessionHistory has more than 50".into());
    }
    for e in list {
        let e = record(e, "a session history entry")?;
        id(required(&e, "id")?, "id")?;
        text(required(&e, "role")?, "role", 0, 40)?;
        text(required(&e, "model")?, "model", 0, 40)?;
        int(required(&e, "lane")?, "lane", 0)?;
        number(required(&e, "startedAt")?, "startedAt", f64::MIN)?;
        optional(&e, "costUsd", |v| number(v, "costUsd", 0.0))?;
        optional(&e, "tokens", |v| number(v, "tokens", 0.0))?;
    }
    Ok(())
}

fn environments(v: &Value) -> Check {
    let e = object(v, "environments")?;
    unknown_keys(
        e,
        &["staging", "production", "behindCount", "deploying", "promoting"],
        "environments",
    )?;
    for key in ["staging", "production"] {
        let env = object(required(e, key)?, key)?;
        unknown_keys(env, &["branch", "url"], key)?;
        text(required(env, "branch")?, "branch", 1, 255)?;
        optional(env, "url", |v| web_url(v, "url"))?;
    }
    optional(e, "behindCount", |v| int(v, "behindCount", 0))?;
    for key in ["deploying", "promoting"] {
        optional(e, key, |v| {
            let list = v.as_array().ok_or("must be a list")?;
            if list.len() > 500 {
                return Err("more than 500".into());
            }
            list.iter().try_for_each(|i| text(i, "an id", 1, 200))
        })?;
    }
    Ok(())
}

const STATE_KEYS: &[&str] = &[
    "orchestrator",
    "usage",
    "milestones",
    "items",
    "sessions",
    "sessionHistory",
    "lanes",
    "addedLane",
    "mainPlaying",
    "playingItem",
    "drafts",
    "asks",
    "audiences",
    "manualApprovals",
    "playtests",
    "release",
    "ideas",
    "commandsWaiting",
    "phone",
    "teamBranch",
    "environments",
    "liveAt",
    "goingToProduction",
    "releaseCandidate",
    "rollouts",
    "rolloutRefused",
    "rolloutWaiting",
    "rolloutBehind",
    "leased",
    "waiting",
    "repoId",
    "projectBacklog",
    "codeHost",
    "folderProject",
    "yourChanges",
    "changeWaits",
    "codeParts",
    "linkSharedBacklog",
    "deprecationNotes",
    "deprecations",
    "deprecationDays",
    "settings",
    "lighthouse",
    "scan",
    "vet",
];

/// What a patch may also carry (`patchState`): the ids of items, sessions and drafts that are gone.
const PATCH_ONLY_KEYS: &[&str] = &["removed"];

/// `snapshotState` (cloud.ts:185). `patch` makes every key optional (a patch sends only what changed).
fn state(v: &Value, patch: bool) -> Check {
    let s = object(v, "state")?;
    if patch {
        unknown_keys(s, &[STATE_KEYS, PATCH_ONLY_KEYS].concat(), "patch")?;
        optional(s, "removed", |v| {
            let r = object(v, "removed")?;
            unknown_keys(r, &["items", "sessions", "drafts"], "removed")?;
            for key in ["items", "sessions", "drafts"] {
                optional(r, key, |v| {
                    v.as_array()
                        .ok_or("must be a list")?
                        .iter()
                        .try_for_each(|i| id(i, "an id"))
                })?;
            }
            Ok(())
        })?;
    } else {
        unknown_keys(s, STATE_KEYS, "state")?;
    }
    let need = |key: &str| -> Result<Option<&Value>, String> {
        match s.get(key) {
            Some(v) => Ok(Some(v)),
            None if patch => Ok(None),
            None => Err(format!("{key} is missing")),
        }
    };
    if let Some(o) = need("orchestrator")? {
        let o = object(o, "orchestrator")?;
        one_of(
            required(o, "status")?,
            "orchestrator.status",
            &["setup", "running", "paused", "waiting_for_limit"],
        )?;
    }
    if let Some(u) = need("usage")? {
        object(u, "usage")?;
    }
    if let Some(m) = need("milestones")? {
        for m in m.as_array().ok_or("milestones must be a list")? {
            let m = object(m, "a milestone")?;
            id(required(m, "id")?, "milestone.id")?;
            text(required(m, "title")?, "milestone.title", 0, usize::MAX)?;
        }
    }
    if let Some(items) = need("items")? {
        for i in items.as_array().ok_or("items must be a list")? {
            let i = object(i, "an item")?;
            id(required(i, "id")?, "item.id")?;
            text(required(i, "title")?, "item.title", 0, usize::MAX)?;
            text(required(i, "state")?, "item.state", 0, usize::MAX)?;
        }
    }
    if let Some(sessions) = need("sessions")? {
        for x in sessions.as_array().ok_or("sessions must be a list")? {
            let x = object(x, "a session")?;
            id(required(x, "id")?, "session.id")?;
            text(required(x, "itemId")?, "session.itemId", 0, usize::MAX)?;
            if x.contains_key("events") {
                return Err("a session goes up without its events (toSnapshot)".into());
            }
        }
    }
    optional(s, "sessionHistory", session_history)?;
    if let Some(lanes) = need("lanes")? {
        for l in lanes.as_array().ok_or("lanes must be a list")? {
            int(required(object(l, "a lane")?, "k")?, "lane.k", 0)?;
        }
    }
    optional(s, "addedLane", |v| int(v, "addedLane", 1))?;
    if let Some(m) = need("mainPlaying")? {
        boolean(m, "mainPlaying")?;
    }
    optional(s, "playingItem", |v| text(v, "playingItem", 0, usize::MAX))?;
    for key in ["drafts", "asks"] {
        if let Some(list) = need(key)? {
            for d in list.as_array().ok_or("must be a list")? {
                let d = object(d, key)?;
                id(required(d, "id")?, "id")?;
                text(required(d, "state")?, "state", 0, usize::MAX)?;
            }
        }
    }
    optional(s, "commandsWaiting", |v| {
        let c = object(v, "commandsWaiting")?;
        unknown_keys(c, &["version", "text", "by"], "commandsWaiting")?;
        int(required(c, "version")?, "version", 1)?;
        text(required(c, "text")?, "text", 0, 2000)?;
        match required(c, "by")? {
            Value::Null => Ok(()),
            by => text(by, "by", 0, 100),
        }
    })?;
    optional(s, "phone", |v| {
        let p = object(v, "phone")?;
        text(required(p, "appId")?, "appId", 0, 200)?;
        let phones = required(p, "phones")?.as_array().ok_or("phones must be a list")?;
        if phones.len() > 50 {
            return Err("more than 50 phones".into());
        }
        for ph in phones {
            let ph = object(ph, "a phone")?;
            text(required(ph, "serial")?, "serial", 0, 200)?;
            text(required(ph, "model")?, "model", 0, 200)?;
            text(required(ph, "state")?, "state", 0, 50)?;
        }
        Ok(())
    })?;
    optional(s, "environments", environments)?;
    optional(s, "liveAt", |v| web_url(v, "liveAt"))?;
    optional(s, "repoId", |v| int(v, "repoId", 1))?;
    optional(s, "deprecationDays", |v| match v.as_i64() {
        Some(n) if (1..=365).contains(&n) => Ok(()),
        _ => Err("deprecationDays must be 1 to 365".into()),
    })?;
    optional(s, "linkSharedBacklog", |v| {
        (v == &Value::Bool(true))
            .then_some(())
            .ok_or_else(|| "must be true".to_string())
    })?;
    optional(s, "projectBacklog", |v| {
        (v == &Value::Bool(true))
            .then_some(())
            .ok_or_else(|| "must be true".to_string())
    })?;
    optional(s, "codeHost", |v| one_of(v, "codeHost", &["gitlab", "remote", "local"]))?;
    optional(s, "settings", |v| {
        let r = object(v, "settings")?;
        text(required(r, "projectType")?, "projectType", 0, usize::MAX)?;
        text(required(r, "playtests")?, "playtests", 0, usize::MAX)
    })?;
    Ok(())
}

fn session_events(o: &Map<String, Value>) -> Check {
    unknown_keys(o, &["type", "sessionId", "events", "full"], "session_events")?;
    id(required(o, "sessionId")?, "sessionId")?;
    for e in required(o, "events")?.as_array().ok_or("events must be a list")? {
        let e = object(e, "an event")?;
        unknown_keys(
            e,
            &["at", "kind", "text", "id", "parent", "ended", "diff", "todos"],
            "event",
        )?;
        number(required(e, "at")?, "at", f64::MIN)?;
        one_of(required(e, "kind")?, "kind", &["tool", "text", "result"])?;
        text(required(e, "text")?, "text", 0, usize::MAX)?;
        optional(e, "id", |v| text(v, "id", 0, 200))?;
        optional(e, "parent", |v| text(v, "parent", 0, 200))?;
        optional(e, "ended", |v| text(v, "ended", 0, 40))?;
        optional(e, "diff", |v| {
            let d = object(v, "diff")?;
            text(required(d, "path")?, "path", 0, 1000)?;
            int(required(d, "added")?, "added", i64::MIN)?;
            int(required(d, "removed")?, "removed", i64::MIN)?;
            let lines = required(d, "lines")?.as_array().ok_or("lines must be a list")?;
            if lines.len() > 200 {
                return Err("more than 200 lines".into());
            }
            lines.iter().try_for_each(|l| text(l, "a line", 0, 400))?;
            optional(d, "more", |v| int(v, "more", i64::MIN))
        })?;
        optional(e, "todos", |v| {
            let todos = v.as_array().ok_or("todos must be a list")?;
            if todos.len() > 100 {
                return Err("more than 100 todos".into());
            }
            for t in todos {
                let t = object(t, "a todo")?;
                text(required(t, "text")?, "text", 0, 400)?;
                one_of(required(t, "state")?, "state", &["pending", "active", "done"])?;
            }
            Ok(())
        })?;
    }
    optional(o, "full", |v| boolean(v, "full"))
}

fn timings_report(o: &Map<String, Value>) -> Check {
    unknown_keys(o, &["type", "rows"], "timings_report")?;
    let rows = required(o, "rows")?.as_array().ok_or("rows must be a list")?;
    if rows.is_empty() || rows.len() > 200 {
        return Err("rows must be 1 to 200".into());
    }
    for r in rows {
        let r = object(r, "a timing row")?;
        unknown_keys(
            r,
            &["key", "stage", "at", "ms", "outcome", "model", "cost", "item", "wait"],
            "row",
        )?;
        text(required(r, "key")?, "key", 1, 120)?;
        one_of(
            required(r, "stage")?,
            "stage",
            &["queue", "drafting", "building", "checks", "review", "merge"],
        )?;
        text(required(r, "at")?, "at", 10, 40)?; // min 10: `timingRow.at`
        match required(r, "ms")?.as_i64() {
            Some(ms) if (0..=30 * 24 * 60 * 60 * 1000).contains(&ms) => {}
            _ => return Err("ms must be a whole number of milliseconds up to 30 days".into()),
        }
        one_of(
            required(r, "outcome")?,
            "outcome",
            &["ok", "failed", "limit", "cancelled"],
        )?;
        optional(r, "model", |v| text(v, "model", 0, 60))?;
        optional(r, "cost", |v| match v.as_f64() {
            Some(c) if (0.0..=10000.0).contains(&c) => Ok(()),
            _ => Err("cost must be 0 to 10000".into()),
        })?;
        optional(r, "item", |v| text(v, "item", 1, 100))?;
        optional(r, "wait", |v| {
            one_of(v, "wait", &["queue", "limit", "restart", "ci", "person"])
        })?;
    }
    Ok(())
}

/// Whether the hub takes `msg` from an agent: the first thing it finds wrong, or `Ok`.
pub fn check(msg: &Value) -> Check {
    let o = object(msg, "a message")?;
    let kind = o.get("type").and_then(Value::as_str).ok_or("no type")?;
    match kind {
        "hello" => hello(o),
        "snapshot" => {
            unknown_keys(o, &["type", "state"], "snapshot")?;
            state(required(o, "state")?, false)
        }
        "patch" => {
            unknown_keys(o, &["type", "patch"], "patch")?;
            state(required(o, "patch")?, true)
        }
        "session_events" => session_events(o),
        "timings_report" => timings_report(o),
        "alive" => unknown_keys(o, &["type"], "alive"),
        "pending" => {
            unknown_keys(o, &["type", "id", "until"], "pending")?;
            id(required(o, "id")?, "id")?;
            optional(o, "until", |v| int(v, "until", i64::MIN))
        }
        "chat_report" => {
            unknown_keys(o, &["type", "ask", "conversation"], "chat_report")?;
            required(o, "ask")?;
            optional(o, "conversation", |v| text(v, "conversation", 1, 100))
        }
        "bug_report" => {
            unknown_keys(
                o,
                &["type", "ref", "signature", "title", "details", "version", "sameAs"],
                "bug_report",
            )?;
            id(required(o, "ref")?, "ref")?;
            text(required(o, "signature")?, "signature", 1, 200)?;
            text(required(o, "title")?, "title", 1, 120)?;
            text(required(o, "details")?, "details", 0, 2000)?;
            text(required(o, "version")?, "version", 0, 100)?;
            optional(o, "sameAs", |v| match v.as_str() {
                Some(s)
                    if s.len() == 25
                        && s.starts_with("retaskable-bug-")
                        && s[15..]
                            .bytes()
                            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) =>
                {
                    Ok(())
                }
                _ => Err("sameAs must be a retaskable-bug- id".into()),
            })
        }
        "suggestions_ask" => {
            unknown_keys(o, &["type", "ref"], "suggestions_ask")?;
            id(required(o, "ref")?, "ref")
        }
        "suggestion_verdict" => {
            unknown_keys(
                o,
                &["type", "ref", "id", "verdict", "question", "matches", "why"],
                "suggestion_verdict",
            )?;
            id(required(o, "ref")?, "ref")?;
            int(required(o, "id")?, "id", 1)?;
            one_of(required(o, "verdict")?, "verdict", &["idea", "duplicate", "bin", "ask"])?;
            for key in ["question", "matches", "why"] {
                optional(o, key, |v| text(v, key, 0, 2000))?;
            }
            Ok(())
        }
        "ack" => {
            unknown_keys(o, &["type", "id", "result"], "ack")?;
            id(required(o, "id")?, "id")
        }
        "nack" => {
            unknown_keys(o, &["type", "id", "reason"], "nack")?;
            id(required(o, "id")?, "id")?;
            text(required(o, "reason")?, "reason", 0, 2000)
        }
        other => Err(format!("{other} isn't a message this contract knows")),
    }
}

/// The hub drops a message over 1 MiB (`MAX_STATE_BYTES`).
pub const MAX_STATE_BYTES: usize = 1024 * 1024;

/// What every connection sends goes through this in tests (the connector's send sites), so a message that leaves the
/// agent in any connector test is one the hub takes: it fails the test where the hub would silently drop it.
pub fn sent(msg: &Value) {
    if let Err(e) = check(msg) {
        panic!("the hub would not take this message: {e}: {msg}");
    }
    assert!(
        msg.to_string().len() < MAX_STATE_BYTES,
        "the hub drops a message over 1 MiB"
    );
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn it_refuses_what_the_hub_refuses() {
        let bad_orchestrator = json!({ "type": "snapshot", "state": {
            "orchestrator": { "status": "idle" }, "usage": {}, "milestones": [], "items": [], "sessions": [],
            "lanes": [], "mainPlaying": false, "drafts": [], "asks": [] } });
        assert!(check(&bad_orchestrator).unwrap_err().contains("orchestrator.status"));
        let unknown_key = json!({ "type": "snapshot", "state": {
            "orchestrator": { "status": "running" }, "usage": {}, "milestones": [], "items": [], "sessions": [],
            "lanes": [], "mainPlaying": false, "drafts": [], "asks": [], "restartPending": true } });
        assert!(check(&unknown_key).unwrap_err().contains("state.restartPending"));
        let missing = json!({ "type": "snapshot", "state": { "orchestrator": { "status": "running" } } });
        assert!(check(&missing).unwrap_err().contains("usage is missing"));
        let session_with_events =
            json!({ "type": "patch", "patch": { "sessions": [{ "id": "s", "itemId": "i", "events": [] }] } });
        assert!(check(&session_with_events).is_err());
        let live_at = json!({ "type": "patch", "patch": { "liveAt": "example.com" } });
        assert!(check(&live_at).unwrap_err().contains("web address"));
        let hello_without_backlog = json!({ "type": "hello", "agentVersion": "1", "os": "linux", "machine": "m",
            "capabilities": [], "policy": {}, "project": { "githubRepo": "a/b" } });
        assert!(check(&hello_without_backlog).unwrap_err().contains("backlogPath"));
        let row = |ms: i64| {
            json!({ "type": "timings_report", "rows": [{ "key": "k", "stage": "building",
            "at": "2026-11-16T10:00:00.000Z", "ms": ms, "outcome": "ok" }] })
        };
        assert!(check(&row(5)).is_ok());
        assert!(check(&row(-1)).is_err());
    }
}
