//! The machine's own settings (`MACHINE_KEYS` in the TypeScript agent): at the top of `config.json` in both
//! versions (docs/specs/agent-settings.md). `port`, `poll_interval_sec`, `lanes`, `local_model`, `engine_choice`
//! and `cursor_models` read each computer-only setting's environment variable over the file (section 3.2,
//! `withEnvSettings`).
use std::fmt;

use serde::Deserialize;
use serde_json::Value;

use super::ConfigFile;

pub const DEFAULT_PORT: u16 = 4700;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineConfig {
    #[serde(default = "default_lanes")]
    pub lanes: u32,
    #[serde(default = "default_concurrency")]
    pub concurrency: u32,
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
    #[serde(default = "default_port")]
    pub port: u16,
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
            concurrency: default_concurrency(),
            poll_interval_sec: default_poll_interval_sec(),
            max_resume_context_tokens: default_max_resume_context_tokens(),
            max_turns: default_max_turns(),
            models: ModelsConfig::default(),
            local: LocalModelConfig::default(),
            engine_choice: EngineChoiceConfig::default(),
            cursor: CursorConfig::default(),
            port: default_port(),
            budget: MachineBudget::default(),
            cloud: CloudConfig::default(),
        }
    }
}

fn default_lanes() -> u32 {
    2
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
fn default_port() -> u16 {
    DEFAULT_PORT
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
}

impl Default for ModelsConfig {
    fn default() -> Self {
        Self {
            opus_above_pct: default_opus_above_pct(),
            sonnet_above_pct: default_sonnet_above_pct(),
            pause_below_pct: default_pause_below_pct(),
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
    Local,
}

impl fmt::Display for Engine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Engine::Claude => "claude",
            Engine::Cursor => "cursor",
            Engine::Pi => "pi",
            Engine::Local => "local",
        })
    }
}

pub(crate) fn parse_engine(s: &str) -> Option<Engine> {
    match s.trim().to_ascii_lowercase().as_str() {
        "claude" => Some(Engine::Claude),
        "cursor" => Some(Engine::Cursor),
        "pi" => Some(Engine::Pi),
        "local" => Some(Engine::Local),
        _ => None,
    }
}

/// Reads an engine the lenient way the file allows: missing, `null`, not a string, or a name this
/// agent doesn't know (a typo, a future engine) all read as `None` rather than failing the whole
/// config load.
fn deserialize_lenient_engine<'de, D>(deserializer: D) -> Result<Option<Engine>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Value::deserialize(deserializer)?;
    Ok(raw.as_str().and_then(parse_engine))
}

/// Reads `engineChoice.lanes` the lenient way the file allows: not an array, or any entry that
/// isn't `claude`, `cursor` or `default`, drops the whole list to empty rather than failing the
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
    Default,
}

impl fmt::Display for LaneEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            LaneEngine::Claude => "claude",
            LaneEngine::Cursor => "cursor",
            LaneEngine::Default => "default",
        })
    }
}

fn parse_lane_engine(s: &str) -> Option<LaneEngine> {
    match s.trim().to_ascii_lowercase().as_str() {
        "claude" => Some(LaneEngine::Claude),
        "cursor" => Some(LaneEngine::Cursor),
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

pub fn port(cfg: &ConfigFile, env: impl Fn(&str) -> Option<String>) -> u16 {
    env("RETASKABLE_PORT")
        .and_then(|p| p.parse().ok())
        .filter(|&p| port_in_range(p))
        .or_else(|| Some(cfg.machine.port).filter(|&p| port_in_range(p)))
        .unwrap_or(DEFAULT_PORT)
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
pub fn poll_interval_sec(cfg: &ConfigFile, env: impl Fn(&str) -> Option<String>) -> u32 {
    env("RETASKABLE_POLL_SEC")
        .and_then(|v| v.parse().ok())
        .filter(|&n| poll_interval_in_range(n))
        .unwrap_or(cfg.machine.poll_interval_sec)
}

/// "Lanes": `RETASKABLE_LANES`, a whole number from 1 to 64.
pub fn lanes(cfg: &ConfigFile, env: impl Fn(&str) -> Option<String>) -> u32 {
    env("RETASKABLE_LANES")
        .and_then(|v| v.parse().ok())
        .filter(|&n| (1..=64).contains(&n))
        .unwrap_or(cfg.machine.lanes)
}

/// "Local model": `RETASKABLE_LOCAL_MODEL` turns it on with that model's name, or off with
/// `off`, `false`, `0` or `no`; `RETASKABLE_LOCAL_URL` sets its address. Each is independent of
/// the other and of the file's own value.
pub fn local_model(cfg: &ConfigFile, env: impl Fn(&str) -> Option<String>) -> LocalModelConfig {
    let mut result = cfg.machine.local.clone();
    if let Some(raw) = env("RETASKABLE_LOCAL_MODEL") {
        if is_local_model_off(&raw) {
            result.enabled = false;
        } else if valid_model_name(&raw) {
            result.enabled = true;
            result.model = raw;
        }
    }
    if let Some(url) = env("RETASKABLE_LOCAL_URL")
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
pub fn engine_choice(cfg: &ConfigFile, env: impl Fn(&str) -> Option<String>) -> EngineChoiceConfig {
    match env("RETASKABLE_ENGINES") {
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
fn lanes_of(entries: &[String]) -> Option<Vec<LaneEngine>> {
    if entries.len() == 1 && entries[0].trim().is_empty() {
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
pub fn cursor_models(cfg: &ConfigFile, env: impl Fn(&str) -> Option<String>) -> CursorModels {
    let mut result = sanitize_cursor_models(&cfg.machine.cursor.models);
    if let Some(raw) = env("RETASKABLE_CURSOR_MODELS") {
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
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn with_port(port: u16) -> ConfigFile {
        ConfigFile {
            machine: MachineConfig {
                port,
                ..MachineConfig::default()
            },
            ..ConfigFile::default()
        }
    }

    #[test]
    fn port_prefers_the_env_then_the_file_then_4700() {
        let env = |k: &str| (k == "RETASKABLE_PORT").then(|| "4800".to_string());
        assert_eq!(port(&with_port(4900), env), 4800);
        assert_eq!(port(&with_port(4900), no_env), 4900);
        assert_eq!(port(&ConfigFile::default(), no_env), DEFAULT_PORT);
        let bad = |k: &str| (k == "RETASKABLE_PORT").then(|| "nope".to_string());
        assert_eq!(port(&ConfigFile::default(), bad), DEFAULT_PORT);
        // An unparseable env value falls through to the file, not straight to 4700.
        assert_eq!(port(&with_port(4900), bad), 4900);
    }

    #[test]
    fn a_zero_env_port_is_not_used_either() {
        let zero = |k: &str| (k == "RETASKABLE_PORT").then(|| "0".to_string());
        assert_eq!(port(&with_port(4900), zero), 4900);
        assert_eq!(port(&ConfigFile::default(), zero), DEFAULT_PORT);
    }

    #[test]
    fn a_zero_port_in_the_file_is_not_used_either() {
        // Binding port 0 asks the OS for a random ephemeral port, not the documented
        // fallback, so a zero in config.json is as invalid as a zero env value.
        assert_eq!(port(&with_port(0), no_env), DEFAULT_PORT);
    }

    #[test]
    fn a_privileged_port_below_1024_is_not_used_either() {
        // agent-settings.md section 3.1: the documented valid range is 1024 to 65535,
        // so a sub-1024 port (env or file) is as invalid as zero and falls through too.
        let low_env = |k: &str| (k == "RETASKABLE_PORT").then(|| "80".to_string());
        assert_eq!(port(&with_port(4900), low_env), 4900);
        assert_eq!(port(&ConfigFile::default(), low_env), DEFAULT_PORT);
        assert_eq!(port(&with_port(80), no_env), DEFAULT_PORT);
        assert_eq!(port(&with_port(1024), no_env), 1024);
    }

    #[test]
    fn a_port_at_the_top_of_the_documented_range_is_used() {
        // agent-settings.md section 3.1: the documented valid range runs up to 65535,
        // so the top of the range is as valid as the bottom (already covered at 1024).
        let top_env = |k: &str| (k == "RETASKABLE_PORT").then(|| "65535".to_string());
        assert_eq!(port(&ConfigFile::default(), top_env), 65535);
        assert_eq!(port(&with_port(65535), no_env), 65535);
    }

    #[test]
    fn poll_interval_sec_prefers_the_env_then_the_file_then_the_default() {
        let env = |k: &str| (k == "RETASKABLE_POLL_SEC").then(|| "30".to_string());
        let mut cfg = ConfigFile::default();
        cfg.machine.poll_interval_sec = 20;
        assert_eq!(poll_interval_sec(&cfg, env), 30);
        assert_eq!(poll_interval_sec(&cfg, no_env), 20);
        assert_eq!(poll_interval_sec(&ConfigFile::default(), no_env), 10);
    }

    #[test]
    fn poll_interval_sec_outside_2_to_3600_falls_through_to_the_file() {
        let mut cfg = ConfigFile::default();
        cfg.machine.poll_interval_sec = 20;
        let too_low = |k: &str| (k == "RETASKABLE_POLL_SEC").then(|| "1".to_string());
        let too_high = |k: &str| (k == "RETASKABLE_POLL_SEC").then(|| "3601".to_string());
        let not_a_number = |k: &str| (k == "RETASKABLE_POLL_SEC").then(|| "soon".to_string());
        assert_eq!(poll_interval_sec(&cfg, too_low), 20);
        assert_eq!(poll_interval_sec(&cfg, too_high), 20);
        assert_eq!(poll_interval_sec(&cfg, not_a_number), 20);
        // The documented range's ends are both valid.
        let low_end = |k: &str| (k == "RETASKABLE_POLL_SEC").then(|| "2".to_string());
        let high_end = |k: &str| (k == "RETASKABLE_POLL_SEC").then(|| "3600".to_string());
        assert_eq!(poll_interval_sec(&cfg, low_end), 2);
        assert_eq!(poll_interval_sec(&cfg, high_end), 3600);
    }

    #[test]
    fn lanes_prefers_the_env_then_the_file_then_the_default() {
        let env = |k: &str| (k == "RETASKABLE_LANES").then(|| "8".to_string());
        let mut cfg = ConfigFile::default();
        cfg.machine.lanes = 4;
        assert_eq!(lanes(&cfg, env), 8);
        assert_eq!(lanes(&cfg, no_env), 4);
        assert_eq!(lanes(&ConfigFile::default(), no_env), 2);
    }

    #[test]
    fn lanes_outside_1_to_64_falls_through_to_the_file() {
        let mut cfg = ConfigFile::default();
        cfg.machine.lanes = 4;
        let too_low = |k: &str| (k == "RETASKABLE_LANES").then(|| "0".to_string());
        let too_high = |k: &str| (k == "RETASKABLE_LANES").then(|| "65".to_string());
        assert_eq!(lanes(&cfg, too_low), 4);
        assert_eq!(lanes(&cfg, too_high), 4);
        let low_end = |k: &str| (k == "RETASKABLE_LANES").then(|| "1".to_string());
        let high_end = |k: &str| (k == "RETASKABLE_LANES").then(|| "64".to_string());
        assert_eq!(lanes(&cfg, low_end), 1);
        assert_eq!(lanes(&cfg, high_end), 64);
    }

    #[test]
    fn local_model_env_turns_it_on_with_that_models_name() {
        let env = |k: &str| (k == "RETASKABLE_LOCAL_MODEL").then(|| "llama3.1".to_string());
        let result = local_model(&ConfigFile::default(), env);
        assert!(result.enabled);
        assert_eq!(result.model, "llama3.1");
        // The file's own address is kept; RETASKABLE_LOCAL_URL wasn't set.
        assert_eq!(result.base_url, default_local_base_url());
    }

    #[test]
    fn local_model_env_off_values_turn_it_off() {
        let mut cfg = ConfigFile::default();
        cfg.machine.local.enabled = true;
        cfg.machine.local.model = "llama3.1".to_string();
        for off in ["off", "false", "0", "no", "OFF"] {
            let env = move |k: &str| (k == "RETASKABLE_LOCAL_MODEL").then(|| off.to_string());
            assert!(!local_model(&cfg, env).enabled, "{off} should turn it off");
        }
    }

    #[test]
    fn local_model_env_falls_through_to_the_file_when_invalid() {
        let mut cfg = ConfigFile::default();
        cfg.machine.local.enabled = true;
        cfg.machine.local.model = "llama3.1".to_string();
        // Over 100 characters, and a character outside the documented set.
        let too_long = "a".repeat(101);
        let env = |k: &str| (k == "RETASKABLE_LOCAL_MODEL").then(|| too_long.clone());
        let result = local_model(&cfg, env);
        assert!(result.enabled);
        assert_eq!(result.model, "llama3.1");
        let bad_char = |k: &str| (k == "RETASKABLE_LOCAL_MODEL").then(|| "model name".to_string());
        assert_eq!(local_model(&cfg, bad_char).model, "llama3.1");
    }

    #[test]
    fn local_model_url_env_sets_its_address() {
        let env = |k: &str| (k == "RETASKABLE_LOCAL_URL").then(|| "http://10.0.0.5:11436".to_string());
        assert_eq!(
            local_model(&ConfigFile::default(), env).base_url,
            "http://10.0.0.5:11436"
        );
    }

    #[test]
    fn local_model_url_env_with_no_scheme_or_a_user_falls_through_to_the_file() {
        let cfg = ConfigFile::default();
        let no_scheme = |k: &str| (k == "RETASKABLE_LOCAL_URL").then(|| "10.0.0.5:11436".to_string());
        let with_user = |k: &str| (k == "RETASKABLE_LOCAL_URL").then(|| "http://me:pw@10.0.0.5:11436".to_string());
        assert_eq!(local_model(&cfg, no_scheme).base_url, default_local_base_url());
        assert_eq!(local_model(&cfg, with_user).base_url, default_local_base_url());
    }

    #[test]
    fn engine_choice_missing_is_empty() {
        let result = engine_choice(&ConfigFile::default(), no_env);
        assert_eq!(result.default, None);
        assert_eq!(result.roles, RoleEngines::default());
        assert!(result.lanes.is_empty());
    }

    #[test]
    fn engine_choice_file_values_are_used_with_no_env() {
        let mut cfg = ConfigFile::default();
        cfg.machine.engine_choice.default = Some(Engine::Claude);
        cfg.machine.engine_choice.roles.implementer = Some(Engine::Local);
        cfg.machine.engine_choice.lanes = vec![LaneEngine::Claude, LaneEngine::Cursor];
        let result = engine_choice(&cfg, no_env);
        assert_eq!(result.default, Some(Engine::Claude));
        assert_eq!(result.roles.implementer, Some(Engine::Local));
        assert_eq!(result.lanes, vec![LaneEngine::Claude, LaneEngine::Cursor]);
    }

    #[test]
    fn engine_choice_env_a_bare_engine_replaces_the_files_whole_engine_choice() {
        let mut cfg = ConfigFile::default();
        cfg.machine.engine_choice.roles.implementer = Some(Engine::Cursor);
        let env = |k: &str| (k == "RETASKABLE_ENGINES").then(|| "local".to_string());
        let result = engine_choice(&cfg, env);
        assert_eq!(result.default, Some(Engine::Local));
        // The variable replaces the file's whole engineChoice, not just the default: the file's
        // role override is gone (TypeScript's withEnvSettings: `next.engineChoice = e.engines`).
        assert_eq!(result.roles.implementer, None);
    }

    #[test]
    fn engine_choice_env_a_bare_unknown_value_still_replaces_the_file_with_empty() {
        // `{}` is truthy in the TypeScript agent, so even a value that parses to nothing still
        // replaces the file's engineChoice wholesale rather than leaving it alone.
        let mut cfg = ConfigFile::default();
        cfg.machine.engine_choice.default = Some(Engine::Cursor);
        cfg.machine.engine_choice.roles.implementer = Some(Engine::Local);
        let env = |k: &str| (k == "RETASKABLE_ENGINES").then(|| "steam".to_string());
        assert_eq!(engine_choice(&cfg, env), EngineChoiceConfig::default());
    }

    #[test]
    fn engine_choice_env_pairs_set_default_and_roles() {
        let env = |k: &str| (k == "RETASKABLE_ENGINES").then(|| "default=cursor,implementer=local".to_string());
        let result = engine_choice(&ConfigFile::default(), env);
        assert_eq!(result.default, Some(Engine::Cursor));
        assert_eq!(result.roles.implementer, Some(Engine::Local));
        assert_eq!(result.roles.planner, None);
    }

    #[test]
    fn engine_choice_env_an_invalid_role_value_is_left_out_of_the_replacement() {
        let mut cfg = ConfigFile::default();
        cfg.machine.engine_choice.roles.planner = Some(Engine::Cursor);
        let env = |k: &str| (k == "RETASKABLE_ENGINES").then(|| "planner=steam".to_string());
        // The variable still replaces the file's engineChoice, so the invalid value doesn't fall
        // through to the file's role: the whole thing ends up unset.
        assert_eq!(engine_choice(&cfg, env).roles.planner, None);
    }

    #[test]
    fn engine_choice_env_lanes_sets_them_in_order() {
        let env = |k: &str| {
            (k == "RETASKABLE_ENGINES").then(|| "default=claude,lanes=claude+claude+cursor+cursor".to_string())
        };
        let result = engine_choice(&ConfigFile::default(), env);
        assert_eq!(
            result.lanes,
            vec![
                LaneEngine::Claude,
                LaneEngine::Claude,
                LaneEngine::Cursor,
                LaneEngine::Cursor
            ]
        );
    }

    #[test]
    fn engine_choice_env_lanes_with_any_invalid_entry_is_left_out_whole() {
        let mut cfg = ConfigFile::default();
        cfg.machine.engine_choice.lanes = vec![LaneEngine::Cursor, LaneEngine::Cursor];
        let env = |k: &str| (k == "RETASKABLE_ENGINES").then(|| "lanes=claude+steam+cursor".to_string());
        // One bad entry drops the whole list; since the variable replaces the file's engineChoice
        // entirely, the result is empty, not the file's lanes.
        assert_eq!(engine_choice(&cfg, env).lanes, Vec::<LaneEngine>::new());
    }

    #[test]
    fn engine_choice_env_lanes_with_nothing_after_it_is_an_empty_list() {
        let mut cfg = ConfigFile::default();
        cfg.machine.engine_choice.lanes = vec![LaneEngine::Cursor, LaneEngine::Cursor];
        let env = |k: &str| (k == "RETASKABLE_ENGINES").then(|| "lanes=".to_string());
        assert_eq!(engine_choice(&cfg, env).lanes, Vec::<LaneEngine>::new());
    }

    #[test]
    fn engine_choice_env_lanes_drops_trailing_default_entries() {
        let env = |k: &str| (k == "RETASKABLE_ENGINES").then(|| "lanes=claude+default".to_string());
        assert_eq!(
            engine_choice(&ConfigFile::default(), env).lanes,
            vec![LaneEngine::Claude]
        );
    }

    #[test]
    fn engine_choice_env_lanes_keeps_a_default_entry_that_isnt_trailing() {
        let env = |k: &str| (k == "RETASKABLE_ENGINES").then(|| "lanes=default+claude".to_string());
        assert_eq!(
            engine_choice(&ConfigFile::default(), env).lanes,
            vec![LaneEngine::Default, LaneEngine::Claude]
        );
    }

    #[test]
    fn engine_choice_and_cursor_models_read_from_json_the_way_the_file_spells_them() {
        let json = r#"{
            "engineChoice": {
                "default": "cursor",
                "roles": { "implementer": "local" },
                "lanes": ["claude", "claude", "cursor", "default"]
            },
            "cursor": { "models": { "top": "gpt-5", "implementer": "auto", "scout": "" } }
        }"#;
        let cfg: MachineConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.engine_choice.default, Some(Engine::Cursor));
        assert_eq!(cfg.engine_choice.roles.implementer, Some(Engine::Local));
        assert_eq!(cfg.engine_choice.roles.planner, None);
        // The file's lanes go through the same lenient reading as the environment's: a trailing
        // "default" entry is dropped (lanesOf).
        assert_eq!(
            cfg.engine_choice.lanes,
            vec![LaneEngine::Claude, LaneEngine::Claude, LaneEngine::Cursor]
        );
        let resolved = cursor_models(
            &ConfigFile {
                machine: cfg,
                ..ConfigFile::default()
            },
            no_env,
        );
        assert_eq!(resolved.top, Some("gpt-5".to_string()));
        assert_eq!(resolved.implementer, Some("auto".to_string()));
        // An empty string is no name at all, so it reads as auto (None).
        assert_eq!(resolved.scout, None);
    }

    #[test]
    fn an_unknown_engine_in_the_file_reads_as_none_instead_of_failing() {
        // "pi" is a real engine (engines.md section 13): it must parse, case and all. "steam" is
        // not an engine at all, and a lane holding one must not fail the whole config load either.
        let json = r#"{
            "engineChoice": {
                "default": "PI",
                "roles": { "implementer": "steam" },
                "lanes": ["claude", "steam"]
            }
        }"#;
        let cfg: MachineConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.engine_choice.default, Some(Engine::Pi));
        assert_eq!(cfg.engine_choice.roles.implementer, None);
        // One bad lane entry drops the whole list rather than just that entry.
        assert!(cfg.engine_choice.lanes.is_empty());
    }

    #[test]
    fn cursor_models_missing_is_auto() {
        let result = cursor_models(&ConfigFile::default(), no_env);
        assert_eq!(result.top, None);
        assert_eq!(result.implementer, None);
        assert_eq!(result.scout, None);
    }

    #[test]
    fn cursor_models_a_bad_name_in_the_file_reads_as_auto() {
        let mut cfg = ConfigFile::default();
        cfg.machine.cursor.models.top = Some("--force".to_string());
        assert_eq!(cursor_models(&cfg, no_env).top, None);
    }

    #[test]
    fn cursor_models_env_a_bare_name_sets_every_tier() {
        let env = |k: &str| (k == "RETASKABLE_CURSOR_MODELS").then(|| "gpt-5".to_string());
        let result = cursor_models(&ConfigFile::default(), env);
        assert_eq!(result.top, Some("gpt-5".to_string()));
        assert_eq!(result.implementer, Some("gpt-5".to_string()));
        assert_eq!(result.scout, Some("gpt-5".to_string()));
    }

    #[test]
    fn cursor_models_env_pairs_set_each_tier_by_name() {
        let env = |k: &str| {
            (k == "RETASKABLE_CURSOR_MODELS").then(|| "top=gpt-5,implementer=sonnet,scout=gpt-5-mini".to_string())
        };
        let result = cursor_models(&ConfigFile::default(), env);
        assert_eq!(result.top, Some("gpt-5".to_string()));
        assert_eq!(result.implementer, Some("sonnet".to_string()));
        assert_eq!(result.scout, Some("gpt-5-mini".to_string()));
    }

    #[test]
    fn cursor_models_env_an_invalid_name_falls_through_to_the_file() {
        let mut cfg = ConfigFile::default();
        cfg.machine.cursor.models.top = Some("sonnet".to_string());
        let bare = |k: &str| (k == "RETASKABLE_CURSOR_MODELS").then(|| "a b".to_string());
        assert_eq!(cursor_models(&cfg, bare).top, Some("sonnet".to_string()));
        let pair = |k: &str| (k == "RETASKABLE_CURSOR_MODELS").then(|| "top=--force".to_string());
        assert_eq!(cursor_models(&cfg, pair).top, Some("sonnet".to_string()));
    }
}
