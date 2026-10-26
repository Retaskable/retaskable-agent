//! The machine's own settings (`MACHINE_KEYS` in the TypeScript agent): at the top of `config.json` in both
//! versions (docs/specs/agent-settings.md). `port`, `poll_interval_sec`, `lanes` and `local_model` read each
//! computer-only setting's environment variable over the file (section 3.2, `withEnvSettings`).
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
    /// The Cursor model per tier, `auto` when missing (engines.md section 3.2): kept as written.
    #[serde(default)]
    pub cursor: Option<Value>,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub budget: MachineBudget,
    /// cloud/09-environments.md: "The agent's cloud URL is a setting, `cloud.url` in
    /// `.retaskable/config.json`", `RETASKABLE_CLOUD_URL` overriding it (`cloud::url`).
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
            cursor: None,
            port: default_port(),
            budget: MachineBudget::default(),
            cloud: CloudConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudConfig {
    #[serde(default)]
    pub url: Option<String>,
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineBudget {
    #[serde(default)]
    pub daily_usd: f64,
}

// agent-settings.md section 3.1: "Port: a whole number from 1024 to 65535." Anything below
// that (0 included) is as invalid as an unparseable value and falls through the same way.
fn in_range(p: u16) -> bool {
    p >= 1024
}

pub fn port(cfg: &ConfigFile, env: impl Fn(&str) -> Option<String>) -> u16 {
    env("RETASKABLE_PORT")
        .and_then(|p| p.parse().ok())
        .filter(|&p| in_range(p))
        .or_else(|| Some(cfg.machine.port).filter(|&p| in_range(p)))
        .unwrap_or(DEFAULT_PORT)
}

// agent-settings.md section 3.2 (`withEnvSettings`): each computer-only setting's environment
// variable wins over config.json. "A value that doesn't read ... is left out": an env value
// outside the documented range (or unparseable) falls through to the file's, unvalidated here,
// the same as every other field `load` reads.

/// "How often it checks": `RETASKABLE_POLL_SEC`, 2 to 3600 seconds.
pub fn poll_interval_sec(cfg: &ConfigFile, env: impl Fn(&str) -> Option<String>) -> u32 {
    env("RETASKABLE_POLL_SEC")
        .and_then(|v| v.parse().ok())
        .filter(|&n| (2..=3600).contains(&n))
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

fn valid_model_name(s: &str) -> bool {
    !s.is_empty()
        && s.chars().count() <= 100
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '/' | '-'))
}

/// `http` or `https`, with no user or password in it (an `@` before the path means userinfo).
fn valid_local_url(s: &str) -> bool {
    let Some(authority_and_path) = s.strip_prefix("http://").or_else(|| s.strip_prefix("https://")) else {
        return false;
    };
    let authority = authority_and_path.split('/').next().unwrap_or("");
    !authority.is_empty() && !authority.contains('@')
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
}
