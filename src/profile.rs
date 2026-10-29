//! Who the agent works as (docs/specs/agent/09-web-ui.md section 9.2, "Who the agent works as"):
//! there's no sign-in of its own, so it works as whoever `gh` is signed in as on this machine.
//! Every read calls `gh api user` fresh, so there is no cache for a caller's `?fresh=1` to bypass.
use std::process::Command;

use serde::Deserialize;

/// `gh api user`'s login, name and avatar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub login: String,
    pub name: Option<String>,
    /// Kept only when it's `https://`, never a local path `gh` might otherwise hand back.
    pub avatar_url: Option<String>,
}

/// Why a read failed: `gh` isn't installed, isn't signed in, or answered something this agent
/// can't read.
pub const NOT_SIGNED_IN: &str = "gh isn't signed in: run `gh auth login`";

#[derive(Deserialize)]
struct UserJson {
    login: Option<String>,
    name: Option<String>,
    avatar_url: Option<String>,
}

/// Parses `gh api user`'s JSON into a `Profile`, or `None` when it doesn't even have a login.
fn parse(body: &[u8]) -> Option<Profile> {
    let j: UserJson = serde_json::from_slice(body).ok()?;
    let login = j.login.filter(|l| !l.is_empty())?;
    Some(Profile {
        login,
        name: j.name.filter(|n| !n.is_empty()),
        avatar_url: j.avatar_url.filter(|u| u.starts_with("https://")),
    })
}

/// `gh api user`, or [`NOT_SIGNED_IN`] when `gh` can't answer.
pub fn read() -> Result<Profile, &'static str> {
    let output = Command::new("gh")
        .args(["api", "user"])
        .output()
        .map_err(|_| NOT_SIGNED_IN)?;
    if !output.status.success() {
        return Err(NOT_SIGNED_IN);
    }
    parse(&output.stdout).ok_or(NOT_SIGNED_IN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_login_name_and_an_https_avatar() {
        let body = br#"{"login":"octocat","name":"The Octocat","avatar_url":"https://avatars.example/o.png"}"#;
        assert_eq!(
            parse(body),
            Some(Profile {
                login: "octocat".to_string(),
                name: Some("The Octocat".to_string()),
                avatar_url: Some("https://avatars.example/o.png".to_string()),
            })
        );
    }

    #[test]
    fn drops_a_non_https_avatar() {
        let body = br#"{"login":"octocat","avatar_url":"file:///etc/passwd"}"#;
        assert_eq!(parse(body).unwrap().avatar_url, None);
    }

    #[test]
    fn a_missing_name_is_none() {
        let body = br#"{"login":"octocat"}"#;
        let p = parse(body).unwrap();
        assert_eq!(p.name, None);
        assert_eq!(p.avatar_url, None);
    }

    #[test]
    fn no_login_is_none() {
        assert_eq!(parse(br#"{"name":"The Octocat"}"#), None);
    }

    #[test]
    fn unreadable_json_is_none() {
        assert_eq!(parse(b"not json"), None);
    }
}
