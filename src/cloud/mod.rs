//! The agent's outbound link to retaskable.com (Retaskable.com's docs/specs/cloud.md and
//! docs/specs/environments.md; trimmed here to docs/specs/tray/03-one-website-three-environments.md,
//! "one connection and one token per environment, nothing crosses", and docs/specs/agent/09-web-ui.md's
//! "the computer's link state"). `link` is the device-authorization flow that gets this machine its
//! refresh token for one environment; `credentials` is where that token is kept. The WebSocket
//! connector that uses it to report this computer's work (cloud/05-the-connection.md, "the
//! connection") is a later slice, as is linking more than one environment at once
//! (environments/03-one-agent-every-cloud.md).
pub mod credentials;
pub mod link;

/// cloud/09-environments.md: production is the default cloud every agent links to unless told
/// otherwise.
pub const DEFAULT_URL: &str = "https://retaskable.com";

/// Staging's cloud URL (tray/02 decision 7; environments.md section 3).
pub const STAGING_URL: &str = "https://staging.retaskable.com";

/// Local's cloud URL: `wrangler dev` (tray/02 decision 1).
pub const LOCAL_URL: &str = "http://localhost:8787";

/// The cloud this agent links to: `RETASKABLE_CLOUD_URL`, else the config file's `cloud.url`,
/// else [`DEFAULT_URL`] (cloud/09-environments.md: "The agent's cloud URL is a setting, `cloud.url`
/// in `.retaskable/config.json` ... with `RETASKABLE_CLOUD_URL` overriding it"). No trailing slash.
pub fn url(cfg: &crate::config::ConfigFile, env: impl Fn(&str) -> Option<String>) -> String {
    let raw = env("RETASKABLE_CLOUD_URL")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| cfg.machine.cloud.url.clone())
        .unwrap_or_else(|| DEFAULT_URL.to_string());
    raw.trim_end_matches('/').to_string()
}

/// The URL for `retaskable-agent link [environment]` (tray/02 decision 7, tray/04):
/// `production`, `staging` or `local`. With no environment, [`url`] (config / env / production).
/// Any other name, or more than one argument, is refused so a token is never saved under the
/// wrong cloud (tray/03: one connection and one token per environment).
pub fn link_url(
    link_args: &[String],
    cfg: &crate::config::ConfigFile,
    env: impl Fn(&str) -> Option<String>,
) -> Result<String, String> {
    match link_args {
        [] => Ok(url(cfg, env)),
        [name] => match name.as_str() {
            "production" => Ok(DEFAULT_URL.to_string()),
            "staging" => Ok(STAGING_URL.to_string()),
            "local" => Ok(LOCAL_URL.to_string()),
            other => Err(format!(
                "unknown environment '{other}': use production, staging, or local"
            )),
        },
        _ => Err("usage: retaskable-agent link [production|staging|local]".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConfigFile;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn url_defaults_then_the_file_then_the_env_wins_and_trims_a_trailing_slash() {
        assert_eq!(url(&ConfigFile::default(), no_env), DEFAULT_URL);

        let mut cfg = ConfigFile::default();
        cfg.machine.cloud.url = Some("https://staging.retaskable.com/".to_string());
        assert_eq!(url(&cfg, no_env), "https://staging.retaskable.com");

        let env = |k: &str| (k == "RETASKABLE_CLOUD_URL").then(|| "http://localhost:8787".to_string());
        assert_eq!(url(&cfg, env), "http://localhost:8787");

        let blank = |k: &str| (k == "RETASKABLE_CLOUD_URL").then(|| "   ".to_string());
        assert_eq!(url(&cfg, blank), "https://staging.retaskable.com");
    }

    #[test]
    fn link_url_with_no_environment_uses_url() {
        let mut cfg = ConfigFile::default();
        cfg.machine.cloud.url = Some("https://staging.retaskable.com".to_string());
        assert_eq!(link_url(&[], &cfg, no_env).unwrap(), STAGING_URL);

        let env = |k: &str| (k == "RETASKABLE_CLOUD_URL").then(|| "http://localhost:8787".to_string());
        assert_eq!(link_url(&[], &cfg, env).unwrap(), LOCAL_URL);
    }

    #[test]
    fn link_url_maps_each_named_environment_and_ignores_config() {
        let mut cfg = ConfigFile::default();
        cfg.machine.cloud.url = Some("https://example.com".to_string());
        let env = |k: &str| (k == "RETASKABLE_CLOUD_URL").then(|| "https://example.com".to_string());

        assert_eq!(link_url(&["production".into()], &cfg, env).unwrap(), DEFAULT_URL);
        assert_eq!(link_url(&["staging".into()], &cfg, no_env).unwrap(), STAGING_URL);
        assert_eq!(link_url(&["local".into()], &cfg, no_env).unwrap(), LOCAL_URL);
    }

    #[test]
    fn link_url_refuses_an_unknown_environment_or_extra_args() {
        let cfg = ConfigFile::default();
        let err = link_url(&["prod".into()], &cfg, no_env).unwrap_err();
        assert!(err.contains("unknown environment 'prod'"), "{err}");
        assert!(err.contains("production, staging, or local"), "{err}");

        let err = link_url(&["staging".into(), "extra".into()], &cfg, no_env).unwrap_err();
        assert!(err.contains("usage:"), "{err}");
    }
}
