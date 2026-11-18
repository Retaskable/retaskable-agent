//! The variables a person may add beyond a program's known secrets (agent-settings.md section 3.3, decided
//! 2026-11-16): any name that is a plain environment variable name and not one that would break the agent or hand a
//! person's text the means to run code in it, and a value with nothing in it a process environment can't hold.
//! The page mirrors these rules from `rules_json`, but this is the authority: every save and `secret set` is checked
//! here. A name is compared without regard to case, as Windows does.
use serde_json::{Value, json};

pub const MAX_NAME_LEN: usize = 128;
/// A custom variable's value, in bytes. A save is one request of at most 16 KB, so a few of these fit in one.
pub const MAX_VALUE_LEN: usize = 8192;
/// How many custom variables one program keeps.
pub const MAX_CUSTOM: usize = 64;

/// The names refused, and why. `exact` entries match a whole name, `prefix` entries the start of one.
struct Refusal {
    names: &'static [&'static str],
    prefixes: &'static [&'static str],
    why: &'static str,
}

const REFUSED: [Refusal; 4] = [
    Refusal {
        names: &[
            "PATH",
            "PATHEXT",
            "HOME",
            "USER",
            "USERNAME",
            "LOGNAME",
            "SHELL",
            "USERPROFILE",
            "APPDATA",
            "LOCALAPPDATA",
            "PROGRAMDATA",
            "PROGRAMFILES",
            "SYSTEMROOT",
            "WINDIR",
            "COMSPEC",
            "TMPDIR",
            "TEMP",
            "TMP",
            "PWD",
            "OLDPWD",
            "SHLVL",
            "IFS",
            "XDG_RUNTIME_DIR",
            "MSYSTEM",
        ],
        prefixes: &[],
        why: "is a system variable every tool the agent starts depends on: changing it could break them.",
    },
    Refusal {
        names: &[
            "NODE_OPTIONS",
            "NODE_PATH",
            "BASH_ENV",
            "ENV",
            "PS4",
            "PROMPT_COMMAND",
            "EDITOR",
            "VISUAL",
            "PAGER",
            "BROWSER",
            "PYTHONPATH",
            "PYTHONSTARTUP",
            "PYTHONHOME",
            "RUBYOPT",
            "RUBYLIB",
            "PERL5OPT",
            "PERL5LIB",
            "JAVA_TOOL_OPTIONS",
            "_JAVA_OPTIONS",
            "JDK_JAVA_OPTIONS",
            "RUSTC_WRAPPER",
            "RUSTC_WORKSPACE_WRAPPER",
        ],
        prefixes: &["LD_", "DYLD_", "GIT_", "DOCKER_", "BASH_FUNC_"],
        why: "changes what the agent's tools load or run, so it could run other code in them: set it in the shell that starts the agent if you mean it.",
    },
    Refusal {
        names: &[
            "T",
            "REF",
            "SHA",
            "CI",
            "CF_API_TOKEN",
            "CLAUDE_CONFIG_DIR",
            "ENABLE_CLAUDEAI_MCP_SERVERS",
            "ANTHROPIC_BASE_URL",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_SMALL_FAST_MODEL",
            "CLAUDE_CODE_SUBAGENT_MODEL",
            "CLAUDE_CODE_AUTO_COMPACT_WINDOW",
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
        ],
        prefixes: &["CI_", "NPM_CONFIG_", "ANTHROPIC_DEFAULT_"],
        why: "is one the agent sets itself on what it starts, so a saved one would be overwritten or would change how the agent runs it.",
    },
    Refusal {
        names: &[],
        prefixes: &["RETASKABLE_"],
        why: "is in the agent's own namespace: those are set in its environment, never saved here.",
    },
];

fn upper(name: &str) -> String {
    name.to_ascii_uppercase()
}

/// A name's reason for being refused, or `None`.
fn refusal(name: &str) -> Option<&'static str> {
    let up = upper(name);
    REFUSED
        .iter()
        .find(|r| r.names.contains(&up.as_str()) || r.prefixes.iter().any(|p| up.starts_with(p)))
        .map(|r| r.why)
}

/// `^[A-Za-z_][A-Za-z0-9_]*$`, at most `MAX_NAME_LEN`.
fn well_formed(name: &str) -> bool {
    let mut chars = name.chars();
    name.len() <= MAX_NAME_LEN
        && chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Whether `name` may be a custom variable. The error is words for the page; it names the variable only once that is
/// known to be a plain name, so a request's own odd text is never put in a reply.
pub fn check_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Environment variable name is required.".to_string());
    }
    if name.len() > MAX_NAME_LEN {
        return Err(format!("A variable name is at most {MAX_NAME_LEN} characters."));
    }
    if !well_formed(name) {
        return Err("A variable name is letters, digits and underscores, and doesn't start with a digit.".to_string());
    }
    match refusal(name) {
        Some(why) => Err(format!("{name} {why}")),
        None => Ok(()),
    }
}

/// A custom variable's value, already trimmed: 1 to `MAX_VALUE_LEN` bytes, spaces allowed, but no NUL and no other
/// control character (a line break can't be written in the text box, and a process environment can't hold a NUL).
pub fn check_value(name: &str, value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("{name} needs a value (delete the variable to clear it)."));
    }
    if value.len() > MAX_VALUE_LEN {
        return Err(format!("{name}'s value is at most {MAX_VALUE_LEN} bytes."));
    }
    if value.chars().any(|c| c.is_control() && c != '\t') {
        return Err(format!(
            "{name}'s value can't hold a NUL, line break or other control character."
        ));
    }
    Ok(())
}

/// Whether two names are the same variable (to Windows, and so to a computer that may be moved to it).
pub fn same(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// The rules as the page mirrors them, in `GET /api/settings` and each program: no value, only the rules.
pub fn rules_json() -> Value {
    let refused: Vec<Value> = REFUSED
        .iter()
        .flat_map(|r| {
            r.names
                .iter()
                .map(|n| json!({ "name": n, "why": r.why }))
                .chain(r.prefixes.iter().map(|p| json!({ "prefix": p, "why": r.why })))
        })
        .collect();
    json!({ "maxName": MAX_NAME_LEN, "maxValue": MAX_VALUE_LEN, "maxCustom": MAX_CUSTOM, "refused": refused })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_names_pass() {
        for n in ["FOO", "foo_bar", "_x", "A1", "GH_HOST", "MY_API_KEY"] {
            assert_eq!(check_name(n), Ok(()), "{n}");
        }
    }

    #[test]
    fn malformed_names_are_refused_without_echoing_them() {
        for n in ["", "1A", "A-B", "A B", "A=B", "é", "A\0B", "$(x)"] {
            let e = check_name(n).unwrap_err();
            assert!(!e.contains(n) || n.is_empty(), "{n:?} echoed in {e}");
        }
        assert!(check_name(&"A".repeat(128)).is_ok());
        assert!(check_name(&"A".repeat(129)).unwrap_err().contains("128"));
        assert_eq!(check_name("").unwrap_err(), "Environment variable name is required.");
    }

    #[test]
    fn each_class_of_refused_name_is_refused_in_any_case() {
        for n in [
            "PATH",
            "path",
            "HOME",
            "USER",
            "SHELL",
            "LD_PRELOAD",
            "ld_library_path",
            "DYLD_INSERT_LIBRARIES",
            "NODE_OPTIONS",
            "GIT_SSH_COMMAND",
            "DOCKER_HOST",
            "BASH_ENV",
            "RETASKABLE_SECRETS_KEY",
            "RETASKABLE_LANE",
            "RETASKABLE_X",
            "ENABLE_CLAUDEAI_MCP_SERVERS",
            "CLAUDE_CODE_SUBAGENT_MODEL",
            "CF_API_TOKEN",
            "T",
            "npm_config_registry",
            "CI_CARD_LINE",
        ] {
            assert!(check_name(n).unwrap_err().contains(n), "{n}");
        }
    }

    #[test]
    fn values_are_checked() {
        assert_eq!(check_value("A", "hello world"), Ok(()));
        assert_eq!(check_value("A", "tab\there"), Ok(()));
        assert!(check_value("A", "").is_err());
        assert!(check_value("A", "a\0b").is_err());
        assert!(check_value("A", "a\nb").is_err());
        assert!(check_value("A", &"x".repeat(MAX_VALUE_LEN)).is_ok());
        assert!(check_value("A", &"x".repeat(MAX_VALUE_LEN + 1)).is_err());
    }

    #[test]
    fn the_rules_the_page_gets_list_every_refusal() {
        let rules = rules_json();
        let refused = rules["refused"].as_array().unwrap();
        assert!(refused.iter().any(|r| r["name"] == "PATH"));
        assert!(refused.iter().any(|r| r["prefix"] == "LD_"));
        assert!(refused.iter().all(|r| r["why"].is_string()));
    }
}
