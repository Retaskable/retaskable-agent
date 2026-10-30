//! The events in the log, as types (agent-telemetry.md section 3.3). One variant per kind the agent
//! writes or reads, each a payload of optional fields: a field a line lacks stays unset and is not
//! written back, and a field the type does not name rides along in `extra`, so an old or newer line
//! survives a read and a write. A line whose kind is unknown (or whose fields do not fit their kind)
//! is kept whole as `Body::Other`; reading never fails on it.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

macro_rules! payload {
    ($(#[$m:meta])* $name:ident { $($(#[$fm:meta])* $f:ident : $t:ty),* $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
        pub struct $name {
            $( $(#[$fm])* #[serde(default, skip_serializing_if = "Option::is_none")] pub $f: Option<$t>, )*
            /// Fields this type does not name, kept as written.
            #[serde(flatten)]
            pub extra: Map<String, Value>,
        }
    };
}

payload!(
    /// The minute summary.
    Tick { status: String, lanes: u32, busy: Vec<u32>, free: Value, sessions: Value, states: Value, claim: Value }
);
payload!(ItemState {
    item: String,
    from: String,
    to: String,
    note: String,
    lane: u32
});
payload!(LaneTaken {
    item: String,
    lane: u32,
    why: String
});
payload!(LaneFreed {
    item: String,
    lane: u32,
    why: String
});
payload!(Lease {
    item: String,
    change: String,
    state: String
});
payload!(Claim { answer: Value });
payload!(StoreError {
    item: String,
    op: String,
    code: Value,
    error: String
});
payload!(SessionStart {
    item: String,
    lane: u32,
    role: String,
    scout: String,
    engine: String,
    model: String,
    session: String,
    detached: bool,
    attached: bool,
});
payload!(SessionEnd {
    item: String,
    lane: u32,
    role: String,
    scout: String,
    engine: String,
    model: String,
    session: String,
    ms: f64,
    cost: f64,
    ok: bool,
    subtype: String,
    error: String,
    #[serde(rename = "limitHit")]
    limit_hit: Value,
});
payload!(StepStart {
    item: String,
    step: String,
    key: String,
    again: bool
});
payload!(StepEnd {
    item: String, step: String, key: String, ms: f64, ok: bool, outcome: String, checks: Vec<Check>,
});
payload!(Ci {
    item: String,
    state: String,
    ms: f64
});
payload!(Merged {
    item: String,
    pr: u64,
    chore: bool
});
payload!(Error {
    item: String,
    r#where: String,
    error: String
});
payload!(HostCallUndefined {
    cmd: String,
    args: Value
});
payload!(RestartAsked { reason: String, by: String, #[serde(rename = "waitingOn")] waiting_on: Vec<String> });
payload!(RestartWaiting {
    reason: String, #[serde(rename = "waitingOn")] waiting_on: Vec<String>, ms: f64,
});
payload!(RestartExit {
    reason: String,
    by: String,
    #[serde(rename = "drainMs")]
    drain_ms: f64
});
payload!(RestartBack {
    reason: String,
    by: String,
    #[serde(rename = "downMs")]
    down_ms: f64
});
payload!(Anomaly {
    item: String,
    phase: String,
    id: String,
    rule: String,
    text: String,
    ms: f64,
    details: Value,
});
payload!(
    /// One check of a `step_end`.
    Check { name: String, ok: bool }
);

/// One kind of event the log knows, by its `kind` string.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    Tick(Tick),
    ItemState(ItemState),
    LaneTaken(LaneTaken),
    LaneFreed(LaneFreed),
    Lease(Lease),
    Claim(Claim),
    StoreError(StoreError),
    SessionStart(SessionStart),
    SessionEnd(SessionEnd),
    StepStart(StepStart),
    StepEnd(StepEnd),
    Ci(Ci),
    Merged(Merged),
    Error(Error),
    HostCallUndefined(HostCallUndefined),
    RestartAsked(RestartAsked),
    RestartWaiting(RestartWaiting),
    RestartExit(RestartExit),
    RestartBack(RestartBack),
    Anomaly(Anomaly),
}

fn extra_item(extra: &Map<String, Value>) -> Option<&str> {
    extra.get("item").and_then(Value::as_str)
}

impl Event {
    /// The item this event is about, when it names one.
    pub fn item(&self) -> Option<&str> {
        let own = match self {
            Event::ItemState(e) => &e.item,
            Event::LaneTaken(e) => &e.item,
            Event::LaneFreed(e) => &e.item,
            Event::Lease(e) => &e.item,
            Event::StoreError(e) => &e.item,
            Event::SessionStart(e) => &e.item,
            Event::SessionEnd(e) => &e.item,
            Event::StepStart(e) => &e.item,
            Event::StepEnd(e) => &e.item,
            Event::Ci(e) => &e.item,
            Event::Merged(e) => &e.item,
            Event::Error(e) => &e.item,
            Event::Anomaly(e) => &e.item,
            // These name no item of their own; a line may still carry one.
            Event::Tick(e) => return extra_item(&e.extra),
            Event::Claim(e) => return extra_item(&e.extra),
            Event::HostCallUndefined(e) => return extra_item(&e.extra),
            Event::RestartAsked(e) => return extra_item(&e.extra),
            Event::RestartWaiting(e) => return extra_item(&e.extra),
            Event::RestartExit(e) => return extra_item(&e.extra),
            Event::RestartBack(e) => return extra_item(&e.extra),
        };
        own.as_deref()
    }

    /// The `kind` string it is written with.
    pub fn kind(&self) -> String {
        match serde_json::to_value(self) {
            Ok(Value::Object(mut m)) => match m.remove("kind") {
                Some(Value::String(k)) => k,
                _ => String::new(),
            },
            _ => String::new(),
        }
    }

    /// Every field it is written with, `kind` apart.
    pub fn fields(&self) -> Map<String, Value> {
        match serde_json::to_value(self) {
            Ok(Value::Object(mut m)) => {
                m.remove("kind");
                m
            }
            _ => Map::new(),
        }
    }
}

/// What a line holds besides its time and project.
// Known lines are nearly all of them; boxing them would only add an allocation per event.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum Body {
    Known(Event),
    /// A kind this version does not know, or a known kind whose fields do not fit: as written.
    Other {
        kind: String,
        fields: Map<String, Value>,
    },
}

/// One line of the log: when, for which project (the file says; a line may carry it too), what.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// Full ISO-8601 UTC, `YYYY-MM-DDTHH:MM:SS.sssZ` when written by the agent.
    pub at: String,
    pub project: Option<String>,
    pub body: Body,
}

impl Entry {
    pub fn new(at: String, project: Option<String>, event: Event) -> Self {
        Self {
            at,
            project,
            body: Body::Known(event),
        }
    }

    /// The entry a parsed line stands for: an object with a non-empty string `kind` and a string
    /// `at`. Anything else is not an event line.
    pub fn from_value(v: &Value) -> Option<Self> {
        let mut map = v.as_object()?.clone();
        let kind = map.get("kind")?.as_str().filter(|k| !k.is_empty())?.to_string();
        let Some(Value::String(at)) = map.remove("at") else {
            return None;
        };
        let project = match map.get("project") {
            Some(Value::String(_)) => match map.remove("project") {
                Some(Value::String(p)) => Some(p),
                _ => None,
            },
            _ => None,
        };
        let body = match serde_json::from_value::<Event>(Value::Object(map.clone())) {
            Ok(event) => Body::Known(event),
            Err(_) => {
                map.remove("kind");
                Body::Other { kind, fields: map }
            }
        };
        Some(Self { at, project, body })
    }

    /// The line's JSON: `at`, `kind`, `project` when set, and the rest.
    pub fn to_value(&self) -> Value {
        let mut map = match &self.body {
            Body::Known(event) => match serde_json::to_value(event) {
                Ok(Value::Object(m)) => m,
                _ => Map::new(),
            },
            Body::Other { kind, fields } => {
                let mut m = fields.clone();
                m.insert("kind".into(), Value::String(kind.clone()));
                m
            }
        };
        map.insert("at".into(), Value::String(self.at.clone()));
        if let Some(p) = &self.project {
            map.insert("project".into(), Value::String(p.clone()));
        }
        Value::Object(map)
    }

    pub fn kind(&self) -> String {
        match &self.body {
            Body::Known(e) => e.kind(),
            Body::Other { kind, .. } => kind.clone(),
        }
    }

    pub fn event(&self) -> Option<&Event> {
        match &self.body {
            Body::Known(e) => Some(e),
            Body::Other { .. } => None,
        }
    }

    /// The item it is about, when it names one.
    pub fn item(&self) -> Option<&str> {
        match &self.body {
            Body::Known(e) => e.item(),
            Body::Other { fields, .. } => fields.get("item").and_then(Value::as_str),
        }
    }
}
