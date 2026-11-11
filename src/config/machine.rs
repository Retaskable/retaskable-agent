//! The machine's own settings (`MACHINE_KEYS` in the TypeScript agent): at the top of `config.json` in both
//! versions (docs/specs/agent-settings.md). `port`, `poll_interval_sec`, `lanes`, `local_model`, `engine_choice`
//! and `cursor_models` read each computer-only setting's environment variable over the file (section 3.2,
//! `withEnvSettings`).
use crate::env::Env;
use std::fmt;

use serde::Deserialize;
use serde_json::Value;

use super::{ConfigFile, ModelConcurrency};
pub use crate::lane_pool::LaneScaling;

pub const DEFAULT_PORT: u16 = 4700;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineConfig {
    #[serde(default = "default_lanes")]
    pub lanes: u32,
    /// `ciSlots` (agent-ci.md section 2): how many pull requests' CI runs at once in Docker; 0 turns it off.
    #[serde(default = "default_ci_slots")]
    pub ci_slots: u32,
    /// `laneScaling` (04-architecture.md section 4.1); dropped when it isn't a `{ base, belowUsedPct }`.
    #[serde(default, deserialize_with = "deserialize_lenient")]
    pub lane_scaling: Option<LaneScaling>,
    /// `choreLane`, a lane number (section 4.1); dropped when it isn't a positive whole number.
    #[serde(default, deserialize_with = "deserialize_lenient")]
    pub chore_lane: Option<u32>,
    #[serde(default = "default_concurrency")]
    pub concurrency: u32,
    /// `modelConcurrency`: a cap per model (opus, sonnet), none by default.
    #[serde(default)]
    pub model_concurrency: ModelConcurrency,
    #[serde(default = "default_poll_interval_sec")]
    pub poll_interval_sec: u32,
    /// Past this many tokens an implementer's follow-up starts a fresh session rather than resuming.
    #[serde(default = "default_max_resume_context_tokens")]
    pub max_resume_context_tokens: u32,
    /// Claude Code's `--max-turns` for an implementer session.
    #[serde(default = "default_max_turns")]
    pub max_turns: u32,
    #[serde(default)]
    pub models: ModelsConfig,
    #[serde(default)]
    pub local: LocalModelConfig,
    /// Which program runs each kind of work (section 3.1 below).
    #[serde(default)]
    pub engine_choice: EngineChoiceConfig,
    /// The Cursor model per tier, `auto` when missing or invalid (section 3.1 below, engines.md section 3.4).
    #[serde(default)]
    pub cursor: CursorConfig,
    /// `None` when the file sets no port at all, kept apart from an explicit value so [`port`]
    /// can fall back to this build's own default (installers/03-the-bundle-agent-bundle.md
    /// section 3.13: 4701 for a Staging release build) rather than one baked in here.
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub budget: MachineBudget,
    /// The cloud this agent links to (`link::cloud_url`): `cloud.url`, retaskable.com when missing.
    #[serde(default)]
    pub cloud: CloudConfig,
}

impl Default for MachineConfig {
    fn default() -> Self {
        Self {
            lanes: default_lanes(),
            ci_slots: default_ci_slots(),
            lane_scaling: None,
            chore_lane: None,
            concurrency: default_concurrency(),
            model_concurrency: ModelConcurrency::default(),
            poll_interval_sec: default_poll_interval_sec(),
            max_resume_context_tokens: default_max_resume_context_tokens(),
            max_turns: default_max_turns(),
            models: ModelsConfig::default(),
            local: LocalModelConfig::default(),
            engine_choice: EngineChoiceConfig::default(),
            cursor: CursorConfig::default(),
            port: None,
            budget: MachineBudget::default(),
            cloud: CloudConfig::default(),
        }
    }
}

fn default_lanes() -> u32 {
    2
}
fn default_ci_slots() -> u32 {
    1
}
fn default_concurrency() -> u32 {
    1
}
fn default_poll_interval_sec() -> u32 {
    10
}
fn default_max_resume_context_tokens() -> u32 {
    150_000
}
fn default_max_turns() -> u32 {
    300
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsConfig {
    #[serde(default = "default_opus_above_pct")]
    pub opus_above_pct: u32,
    #[serde(default = "default_sonnet_above_pct")]
    pub sonnet_above_pct: u32,
    #[serde(default = "default_pause_below_pct")]
    pub pause_below_pct: u32,
    #[serde(default = "default_week_cutoff_pct")]
    pub week_cutoff_pct: u32,
}

impl Default for ModelsConfig {
    fn default() -> Self {
        Self {
            opus_above_pct: default_opus_above_pct(),
            sonnet_above_pct: default_sonnet_above_pct(),
            pause_below_pct: default_pause_below_pct(),
            week_cutoff_pct: default_week_cutoff_pct(),
        }
    }
}

fn default_opus_above_pct() -> u32 {
    50
}
fn default_sonnet_above_pct() -> u32 {
    20
}
fn default_pause_below_pct() -> u32 {
    5
}
fn default_week_cutoff_pct() -> u32 {
    95
}

/// A local Anthropic-compatible model server (spec section 7.2). Missing or `enabled: false` means off.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalModelConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_local_base_url")]
    pub base_url: String,
    #[serde(default)]
    pub model: String,
}

impl Default for LocalModelConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: default_local_base_url(),
            model: String::new(),
        }
    }
}

fn default_local_base_url() -> String {
    "http://localhost:11436".to_string()
}

/// Which program runs a kind of work: `engineChoice`'s `default` and each role in `roles`
/// (agent-settings.md section 3.1, engines.md section 3.1, 13). `local` is Claude Code pinned to
/// the local model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    Claude,
    Cursor,
    Pi,
    Codex,
    Local,
}

impl fmt::Display for Engine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Engine::Claude => "claude",
            Engine::Cursor => "cursor",
            Engine::Pi => "pi",
            Engine::Codex => "codex",
            Engine::Local => "local",
        })
    }
}

pub(crate) fn parse_engine(s: &str) -> Option<Engine> {
    match s.trim().to_ascii_lowercase().as_str() {
        "claude" => Some(Engine::Claude),
        "cursor" => Some(Engine::Cursor),
        "pi" => Some(Engine::Pi),
        "codex" => Some(Engine::Codex),
        "local" => Some(Engine::Local),
        _ => None,
    }
}

/// Reads an engine the lenient way the file allows: missing, `null`, not a string, or a name this
/// agent doesn't know (a typo, a future engine) all read as `None` rather than failing the whole
/// config load.
/// An optional setting that's left out, rather than failing the whole config load, when it doesn't read as `T`
/// (a zero `choreLane` included, since lanes count from 1).
fn deserialize_lenient<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned + Lenient,
{
    let raw = Value::deserialize(deserializer)?;
    Ok(serde_json::from_value::<T>(raw).ok().filter(Lenient::usable))
}

/// Whether a value read for a lenient setting is one it can use.
trait Lenient {
    fn usable(&self) -> bool;
}

impl Lenient for u32 {
    fn usable(&self) -> bool {
        *self > 0
    }
}

impl Lenient for LaneScaling {
    fn usable(&self) -> bool {
        self.below_used_pct.is_finite()
    }
}

fn deserialize_lenient_engine<'de, D>(deserializer: D) -> Result<Option<Engine>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Value::deserialize(deserializer)?;
    Ok(raw.as_str().and_then(parse_engine))
}

/// Reads `engineChoice.lanes` the lenient way the file allows: not an array, or any entry that
/// isn't `claude`, `cursor`, `codex` or `default`, drops the whole list to empty rather than failing the
/// whole config load (mirrors `lanes_of`, used for the same list read from the environment).
fn deserialize_lenient_lanes<'de, D>(deserializer: D) -> Result<Vec<LaneEngine>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Value::deserialize(deserializer)?;
    let Some(arr) = raw.as_array() else {
        return Ok(Vec::new());
    };
    let entries: Vec<String> = arr.iter().map(|v| v.as_str().unwrap_or_default().to_string()).collect();
    Ok(lanes_of(&entries).unwrap_or_default())
}

/// One entry of `engineChoice.lanes`: the implementer's engine for that lane, lane 1 first.
/// `Default` follows the implementer's own engine (`default` above, or its role override).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LaneEngine {
    Claude,
    Cursor,
    Codex,
    Default,
}

impl fmt::Display for LaneEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            LaneEngine::Claude => "claude",
            LaneEngine::Cursor => "cursor",
            LaneEngine::Codex => "codex",
            LaneEngine::Default => "default",
        })
    }
}

fn parse_lane_engine(s: &str) -> Option<LaneEngine> {
    match s.trim().to_ascii_lowercase().as_str() {
        "claude" => Some(LaneEngine::Claude),
        "cursor" => Some(LaneEngine::Cursor),
        "codex" => Some(LaneEngine::Codex),
        "default" => Some(LaneEngine::Default),
        _ => None,
    }
}

/// `engineChoice.roles`: the default's override per group of roles (engines.md section 3.1).
/// Missing (`None`) follows `default`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoleEngines {
    #[serde(default, deserialize_with = "deserialize_lenient_engine")]
    pub planner: Option<Engine>,
    #[serde(default, deserialize_with = "deserialize_lenient_engine")]
    pub implementer: Option<Engine>,
    #[serde(default, deserialize_with = "deserialize_lenient_engine")]
    pub review: Option<Engine>,
    #[serde(default, deserialize_with = "deserialize_lenient_engine")]
    pub drafter: Option<Engine>,
    #[serde(default, deserialize_with = "deserialize_lenient_engine")]
    pub chat: Option<Engine>,
    #[serde(default, deserialize_with = "deserialize_lenient_engine")]
    pub writer: Option<Engine>,
}

/// `engineChoice` (agent-settings.md section 3.1): missing means everything on Claude Code.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineChoiceConfig {
    #[serde(default, deserialize_with = "deserialize_lenient_engine")]
    pub default: Option<Engine>,
    #[serde(default)]
    pub roles: RoleEngines,
    #[serde(default, deserialize_with = "deserialize_lenient_lanes")]
    pub lanes: Vec<LaneEngine>,
}

/// `cursor.models`: the Cursor model name per tier, `None` meaning `auto`, Cursor's own pick
/// (engines.md section 3.4). A name read from the file that isn't a valid model name is dropped
/// to `auto` the same way an invalid one from the environment is.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorModels {
    #[serde(default)]
    pub top: Option<String>,
    #[serde(default)]
    pub implementer: Option<String>,
    #[serde(default)]
    pub scout: Option<String>,
}

/// A name read with `top`/`implementer`/`scout` empty or invalid, however it got there (a
/// hand-edited file, or the struct built directly), reads as `auto`.
fn sanitize_cursor_models(models: &CursorModels) -> CursorModels {
    let clean = |name: &Option<String>| name.clone().filter(|s| valid_cursor_model_name(s));
    CursorModels {
        top: clean(&models.top),
        implementer: clean(&models.implementer),
        scout: clean(&models.scout),
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorConfig {
    #[serde(default)]
    pub models: CursorModels,
}

/// "A name is one or more of letters, digits, `_`, `.`, `:`, `/` and `-`, not starting with `-`"
/// (engines.md section 3.4, so `--force` can't be slipped in as a model).
pub(crate) fn valid_cursor_model_name(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('-')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '/' | '-'))
}

/// `cloud`: only its `url` is read here (`link::cloud_url`).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct CloudConfig {
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineBudget {
    #[serde(default)]
    pub daily_usd: f64,
}

// agent-settings.md section 3.1: "Port: a whole number from 1024 to 65535." Anything below
// that (0 included) is as invalid as an unparseable value and falls through the same way.
pub(crate) fn port_in_range(p: u16) -> bool {
    p >= 1024
}

/// `default` is this build's own fallback (installers/03-the-bundle-agent-bundle.md section
/// 3.13: a Staging release build's own 4701, [`DEFAULT_PORT`] otherwise, from
/// `release::default_port`), used only once neither `RETASKABLE_PORT` nor the file's own `port`
/// says a valid one.
pub fn port(cfg: &ConfigFile, env: &Env, default: u16) -> u16 {
    env.get("RETASKABLE_PORT")
        .and_then(|p| p.parse().ok())
        .filter(|&p| port_in_range(p))
        .or_else(|| cfg.machine.port.filter(|&p| port_in_range(p)))
        .unwrap_or(default)
}

/// "How often it checks": 2 to 3600 seconds (agent-settings.md section 3.1), the settings page's
/// save validates the same range the environment variable does.
pub(crate) fn poll_interval_in_range(n: u32) -> bool {
    (2..=3600).contains(&n)
}

// agent-settings.md section 3.2 (`withEnvSettings`): each computer-only setting's environment
// variable wins over config.json. "A value that doesn't read ... is left out": an env value
// outside the documented range (or unparseable) falls through to the file's, unvalidated here,
// the same as every other field `load` reads.

/// "How often it checks": `RETASKABLE_POLL_SEC`, 2 to 3600 seconds.
pub fn poll_interval_sec(cfg: &ConfigFile, env: &Env) -> u32 {
    env.get("RETASKABLE_POLL_SEC")
        .and_then(|v| v.parse().ok())
        .filter(|&n| poll_interval_in_range(n))
        .unwrap_or(cfg.machine.poll_interval_sec)
}

/// "Lanes": `RETASKABLE_LANES`, a whole number from 1 to 64.
pub fn lanes(cfg: &ConfigFile, env: &Env) -> u32 {
    env.get("RETASKABLE_LANES")
        .and_then(|v| v.parse().ok())
        .filter(|&n| (1..=64).contains(&n))
        .unwrap_or(cfg.machine.lanes)
}

pub(crate) fn ci_slots_in_range(n: u32) -> bool {
    n <= 8
}

/// "CI slots": `RETASKABLE_CI_SLOTS`, a whole number from 0 to 8 (0 turns agent CI off). A file value out of range
/// is the default, 1.
pub fn ci_slots(cfg: &ConfigFile, env: &Env) -> u32 {
    env.get("RETASKABLE_CI_SLOTS")
        .and_then(|v| v.parse().ok())
        .filter(|&n| ci_slots_in_range(n))
        .unwrap_or(if ci_slots_in_range(cfg.machine.ci_slots) {
            cfg.machine.ci_slots
        } else {
            default_ci_slots()
        })
}

/// "Local model": `RETASKABLE_LOCAL_MODEL` turns it on with that model's name, or off with
/// `off`, `false`, `0` or `no`; `RETASKABLE_LOCAL_URL` sets its address. Each is independent of
/// the other and of the file's own value.
pub fn local_model(cfg: &ConfigFile, env: &Env) -> LocalModelConfig {
    let mut result = cfg.machine.local.clone();
    if let Some(raw) = env.get("RETASKABLE_LOCAL_MODEL") {
        if is_local_model_off(&raw) {
            result.enabled = false;
        } else if valid_model_name(&raw) {
            result.enabled = true;
            result.model = raw;
        }
    }
    if let Some(url) = env.get("RETASKABLE_LOCAL_URL")
        && valid_local_url(&url)
    {
        result.base_url = url;
    }
    result
}

fn is_local_model_off(raw: &str) -> bool {
    matches!(raw.to_ascii_lowercase().as_str(), "off" | "false" | "0" | "no")
}

pub(crate) fn valid_model_name(s: &str) -> bool {
    !s.is_empty()
        && s.chars().count() <= 100
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '/' | '-'))
}

/// `http` or `https`, with no user or password in it (an `@` before the path means userinfo).
pub(crate) fn valid_local_url(s: &str) -> bool {
    let Some(authority_and_path) = s.strip_prefix("http://").or_else(|| s.strip_prefix("https://")) else {
        return false;
    };
    let authority = authority_and_path.split('/').next().unwrap_or("");
    !authority.is_empty() && !authority.contains('@')
}

/// "Engines": `RETASKABLE_ENGINES`, one engine for all (`local`) or `name=engine` pairs
/// (`default=cursor,implementer=local`), `lanes` among them as `claude+claude+cursor+cursor`.
/// When the variable is set it replaces the file's whole `engineChoice`, not just the fields it
/// names, the same way the TypeScript agent's `withEnvSettings` does (`next.engineChoice =
/// e.engines`) — even when nothing in the value parses, which leaves every field unset rather
/// than keeping the file's.
pub fn engine_choice(cfg: &ConfigFile, env: &Env) -> EngineChoiceConfig {
    match env.get("RETASKABLE_ENGINES") {
        Some(raw) => {
            let mut result = EngineChoiceConfig::default();
            apply_engines_env(&mut result, &raw);
            result
        }
        None => cfg.machine.engine_choice.clone(),
    }
}

fn apply_engines_env(result: &mut EngineChoiceConfig, raw: &str) {
    if !raw.contains('=') {
        if let Some(engine) = parse_engine(raw) {
            result.default = Some(engine);
        }
        return;
    }
    for part in raw.split(',') {
        let Some((name, value)) = part.split_once('=') else {
            continue;
        };
        let name = name.trim();
        if name == "lanes" {
            let entries: Vec<String> = value.split('+').map(str::to_string).collect();
            if let Some(lanes) = lanes_of(&entries) {
                result.lanes = lanes;
            }
            continue;
        }
        let Some(engine) = parse_engine(value) else { continue };
        match name {
            "default" => result.default = Some(engine),
            "planner" => result.roles.planner = Some(engine),
            "implementer" => result.roles.implementer = Some(engine),
            "review" => result.roles.review = Some(engine),
            "drafter" => result.roles.drafter = Some(engine),
            "chat" => result.roles.chat = Some(engine),
            "writer" => result.roles.writer = Some(engine),
            _ => {}
        }
    }
}

/// TypeScript's `lanesOf` (machineSettings.ts): a lane engine per entry (trimmed and lowercased),
/// trailing `default` entries dropped; `None` when any entry isn't `claude`, `cursor` or `default`
/// — a half-read list would put later lanes on the wrong engines. A single empty entry (an empty
/// string split on `+`, or `lanes=` with nothing after it) is a real empty list, not an invalid
/// one. Used for `lanes` read from both the environment and the file.
pub(crate) fn lanes_of(entries: &[String]) -> Option<Vec<LaneEngine>> {
    if entries.len() == 1 && crate::text::is_blank(&entries[0]) {
        return Some(Vec::new());
    }
    let mut lanes = Vec::with_capacity(entries.len());
    for entry in entries {
        lanes.push(parse_lane_engine(entry)?);
    }
    while lanes.last() == Some(&LaneEngine::Default) {
        lanes.pop();
    }
    Some(lanes)
}

/// "Cursor's models": `RETASKABLE_CURSOR_MODELS`, one model for every tier (`gpt-5`) or each by
/// name (`top=gpt-5,implementer=sonnet,scout=gpt-5-mini`). A name that isn't valid is left out,
/// keeping the file's value (already `auto` if that one wasn't valid either).
pub fn cursor_models(cfg: &ConfigFile, env: &Env) -> CursorModels {
    let mut result = sanitize_cursor_models(&cfg.machine.cursor.models);
    if let Some(raw) = env.get("RETASKABLE_CURSOR_MODELS") {
        apply_cursor_models_env(&mut result, &raw);
    }
    result
}

fn apply_cursor_models_env(result: &mut CursorModels, raw: &str) {
    if !raw.contains('=') {
        let name = raw.trim();
        if valid_cursor_model_name(name) {
            let name = Some(name.to_string());
            result.top = name.clone();
            result.implementer = name.clone();
            result.scout = name;
        }
        return;
    }
    for part in raw.split(',') {
        let Some((name, value)) = part.split_once('=') else {
            continue;
        };
        let value = value.trim();
        if !valid_cursor_model_name(value) {
            continue;
        }
        match name.trim() {
            "top" => result.top = Some(value.to_string()),
            "implementer" => result.implementer = Some(value.to_string()),
            "scout" => result.scout = Some(value.to_string()),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests;
