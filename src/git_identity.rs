//! Git's identity from the linked GitHub account (docs/specs/setup-wizard.md section 2.3), never from an environment
//! variable: the account's name (else its login) and its email (else the no-reply address GitHub gives every account),
//! set as the global `user.name` and `user.email` only where git has none yet, so a person's own identity is never
//! replaced. Called with the account `gh api user` answered, when a GitHub token is saved and at each start with one.
use serde_json::Value;

/// The identity an account gives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub name: String,
    pub email: String,
}

/// The identity of `gh api user`'s JSON: `None` without a login.
pub fn from_user(user: &Value) -> Option<Identity> {
    let login = user["login"].as_str().filter(|l| !l.is_empty())?;
    let name = user["name"].as_str().map(str::trim).filter(|n| !n.is_empty());
    let public = user["email"].as_str().map(str::trim).filter(|e| e.contains('@'));
    let email = match (public, user["id"].as_u64()) {
        (Some(email), _) => email.to_string(),
        (None, Some(id)) => format!("{id}+{login}@users.noreply.github.com"),
        (None, None) => format!("{login}@users.noreply.github.com"),
    };
    Some(Identity {
        name: name.unwrap_or(login).to_string(),
        email,
    })
}

/// How git is asked and told, so a test needs no git.
pub trait Git {
    /// The global value of `key`, if set and not blank.
    fn get(&self, key: &str) -> Option<String>;
    fn set(&self, key: &str, value: &str) -> bool;
}

/// The real `git config --global`.
pub struct RealGit;

impl Git for RealGit {
    fn get(&self, key: &str) -> Option<String> {
        let out = crate::spawn::command("git")
            .args(["config", "--global", "--get", key])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (out.status.success() && !text.is_empty()).then_some(text)
    }

    fn set(&self, key: &str, value: &str) -> bool {
        crate::spawn::command("git")
            .args(["config", "--global", key, value])
            .status()
            .is_ok_and(|s| s.success())
    }
}

/// Sets the global identity from `identity` for each of the two that git has none of. Returns what it set.
pub fn apply(git: &impl Git, identity: &Identity) -> Vec<&'static str> {
    let mut set = Vec::new();
    for (key, value) in [("user.name", &identity.name), ("user.email", &identity.email)] {
        if git.get(key).is_none() && git.set(key, value) {
            set.push(key);
        }
    }
    set
}

/// At start: asks `gh api user` who the saved token is and sets git's missing identity from it. Silent when `gh` isn't
/// signed in (the wizard says so) and never blocks the start: run it off the main thread.
pub fn set_from_gh_account() {
    let Ok(out) = crate::spawn::command("gh").args(["api", "user"]).output() else {
        return;
    };
    if !out.status.success() {
        return;
    }
    let Some(identity) = serde_json::from_slice::<Value>(&out.stdout)
        .ok()
        .and_then(|u| from_user(&u))
    else {
        return;
    };
    let set = apply(&RealGit, &identity);
    if !set.is_empty() {
        tracing::info!("Set git's {} from the GitHub account.", set.join(" and "));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct Fake(RefCell<BTreeMap<String, String>>);

    impl Git for Fake {
        fn get(&self, key: &str) -> Option<String> {
            self.0.borrow().get(key).cloned()
        }
        fn set(&self, key: &str, value: &str) -> bool {
            self.0.borrow_mut().insert(key.into(), value.into());
            true
        }
    }

    #[test]
    fn the_name_and_public_email_when_the_account_has_them() {
        let id = from_user(&json!({"login":"octo","name":" The Octocat ","email":"o@example.com","id":7})).unwrap();
        assert_eq!((id.name.as_str(), id.email.as_str()), ("The Octocat", "o@example.com"));
    }

    #[test]
    fn the_login_and_githubs_no_reply_address_when_it_has_not() {
        let id = from_user(&json!({"login":"octo","name":null,"email":null,"id":7})).unwrap();
        assert_eq!(
            (id.name.as_str(), id.email.as_str()),
            ("octo", "7+octo@users.noreply.github.com")
        );
        let id = from_user(&json!({"login":"octo"})).unwrap();
        assert_eq!(id.email, "octo@users.noreply.github.com");
        assert_eq!(from_user(&json!({"name":"x"})), None);
    }

    #[test]
    fn a_person_s_own_identity_is_left_alone() {
        let git = Fake::default();
        git.set("user.name", "Mine");
        let id = from_user(&json!({"login":"octo","id":7})).unwrap();
        assert_eq!(apply(&git, &id), ["user.email"]);
        assert_eq!(git.get("user.name").as_deref(), Some("Mine"));
        assert_eq!(apply(&git, &id), Vec::<&str>::new());
    }
}
