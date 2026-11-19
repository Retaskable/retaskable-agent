//! The `session_end` events of every engine session, whatever its role (agent-telemetry.md section 3.4). The runner
//! notes each ended session on the scoped row ([`super::sessions::record_session_end`]); this builds its event, which
//! the project's loop writes on its next pass. A claim-mode implementer's sessions are summed per item instead and
//! ride on the one `session_end` its settle writes (`driver/settle.rs`).
use crate::engines::choose::Role;
use crate::engines::engine::{EngineId, SessionResult};
use crate::telemetry::event::SessionEnd;
use crate::telemetry::measure::clip;

use super::sessions::SessionInfo;

/// What a claim-mode implementer's engine sessions on one item came to since the last settle.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Spent {
    pub cost: Option<f64>,
    pub tokens: Option<(u64, u64, u64)>,
    pub ms: i64,
    pub session: String,
    pub engine: Option<EngineId>,
    pub model: String,
}

impl Spent {
    pub fn add(&mut self, info: &SessionInfo, engine: EngineId, result: &SessionResult, ms: i64) {
        self.cost = match (self.cost, result.cost_usd) {
            (None, c) => c,
            (c, None) => c,
            (Some(a), Some(b)) => Some(a + b),
        };
        if let Some(t) = result.tokens {
            let (i, o, c) = self.tokens.unwrap_or_default();
            self.tokens = Some((i + t.input, o + t.output, c + t.cache));
        }
        self.ms += ms.max(0);
        if !result.session_id.is_empty() {
            self.session = result.session_id.clone();
        }
        self.engine = Some(engine);
        self.model = info.model.clone();
    }

    /// The summed numbers on `event`, which the settle has filled with the rest.
    pub fn onto(&self, event: &mut SessionEnd) {
        event.cost = self.cost;
        event.ms = Some(self.ms as f64);
        if let Some(engine) = self.engine {
            event.engine = Some(engine.to_string());
        }
        if !self.model.is_empty() {
            event.model = Some(self.model.clone());
        }
        if !self.session.is_empty() {
            event.session = Some(self.session.clone());
        }
        set_tokens(event, self.tokens);
    }
}

fn set_tokens(event: &mut SessionEnd, tokens: Option<(u64, u64, u64)>) {
    if let Some((input, output, cache)) = tokens {
        event.tokens = Some(input + output);
        event.input_tokens = Some(input);
        event.output_tokens = Some(output);
        event.cache_tokens = Some(cache).filter(|c| *c > 0);
    }
}

/// Whether the session's end is summed for its settle instead of written on its own.
pub fn settles_later(info: &SessionInfo) -> bool {
    info.role == Role::Implementer && info.lane.is_some() && info.run.is_none()
}

/// The event for one ended engine session. `ms` is the time since the session began (or the last one ended in the row).
pub fn ended(info: &SessionInfo, engine: EngineId, result: &SessionResult, ms: i64) -> SessionEnd {
    let mut event = SessionEnd {
        item: Some(info.item_id.clone()),
        lane: info.lane,
        role: Some(info.role.name().to_string()),
        engine: Some(engine.to_string()),
        model: Some(info.model.clone()).filter(|m| !m.is_empty()),
        session: Some(result.session_id.clone()).filter(|s| !s.is_empty()),
        ms: Some(ms.max(0) as f64),
        cost: result.cost_usd,
        ok: Some(result.ok),
        subtype: Some(result.subtype.clone()).filter(|s| !s.is_empty()),
        error: result
            .error
            .as_deref()
            .filter(|_| !result.ok)
            .map(clip)
            .filter(|e| !e.is_empty()),
        limit_hit: result.limit_hit.then_some(serde_json::Value::Bool(true)),
        ..Default::default()
    };
    if info.run.is_some() {
        event.piece = Some(info.item_id.clone());
        event.run = info.run.clone();
    }
    set_tokens(&mut event, result.tokens.map(|t| (t.input, t.output, t.cache)));
    event
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::engine::Tokens;

    fn info(role: Role, lane: Option<u32>, run: Option<&str>) -> SessionInfo {
        SessionInfo {
            project: "p".into(),
            item_id: "item-1".into(),
            title: None,
            role,
            lane,
            engine: EngineId::Claude,
            model: "sonnet".into(),
            run: run.map(String::from),
        }
    }

    fn result(cost: Option<f64>) -> SessionResult {
        SessionResult {
            ok: true,
            subtype: "success".into(),
            cost_usd: cost,
            session_id: "s-1".into(),
            tokens: Some(Tokens {
                input: 10,
                output: 4,
                cache: 100,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn an_ended_session_carries_its_cost_tokens_and_names() {
        let e = ended(
            &info(Role::Reviewer, None, Some("abc")),
            EngineId::Claude,
            &result(Some(0.5)),
            1234,
        );
        let v = serde_json::to_value(crate::telemetry::Event::SessionEnd(e)).unwrap();
        assert_eq!(v["role"], "reviewer");
        assert_eq!(v["engine"], "claude");
        assert_eq!(v["model"], "sonnet");
        assert_eq!(v["cost"], 0.5);
        assert_eq!(v["ms"], 1234.0);
        assert_eq!(
            (
                v["tokens"].as_u64(),
                v["inputTokens"].as_u64(),
                v["outputTokens"].as_u64()
            ),
            (Some(14), Some(10), Some(4))
        );
        assert_eq!(v["cacheTokens"], 100);
        assert_eq!((v["piece"].as_str(), v["run"].as_str()), (Some("item-1"), Some("abc")));
        assert_eq!(v["ok"], true);
    }

    #[test]
    fn a_failed_session_says_why_cut_and_an_unknown_cost_is_left_out() {
        let mut r = result(None);
        r.ok = false;
        r.error = Some("x".repeat(900));
        let e = ended(&info(Role::Scout, None, None), EngineId::Cursor, &r, 5);
        let v = serde_json::to_value(crate::telemetry::Event::SessionEnd(e)).unwrap();
        assert!(v.get("cost").is_none());
        assert!(v["error"].as_str().unwrap().chars().count() <= crate::telemetry::MAX_TEXT);
        assert!(v.get("piece").is_none());
    }

    #[test]
    fn only_a_claim_mode_implementer_settles_later_and_its_sessions_sum() {
        assert!(settles_later(&info(Role::Implementer, Some(1), None)));
        assert!(!settles_later(&info(Role::Implementer, Some(1), Some("r"))));
        assert!(!settles_later(&info(Role::Implementer, None, None)));
        assert!(!settles_later(&info(Role::Reviewer, Some(1), None)));
        let i = info(Role::Implementer, Some(1), None);
        let mut spent = Spent::default();
        spent.add(&i, EngineId::Claude, &result(Some(0.25)), 100);
        spent.add(&i, EngineId::Claude, &result(Some(0.5)), 200);
        let mut e = SessionEnd::default();
        spent.onto(&mut e);
        assert_eq!(
            (e.cost, e.ms, e.tokens, e.cache_tokens),
            (Some(0.75), Some(300.0), Some(28), Some(200))
        );
    }
}
