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
}
