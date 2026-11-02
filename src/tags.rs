//! This computer's tags (computer-tags.md section 3.1; agent-settings.md section 3): what it
//! finds for itself, your overrides in `machine.json`, and `RETASKABLE_TAGS` over both.
use crate::env::Env;
use std::path::Path;
use std::process::Command;

use serde_json::{Map, Value};

/// The most tags a computer has (computer-tags.md: lowercase, 1 to 20 of a-z, 0-9 and "-").
const MAX_TAGS: usize = 20;

/// Your own changes to a computer's found tags, kept in `.retaskable/machine.json`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TagOverrides {
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

impl TagOverrides {
    pub fn from_value(v: &Value) -> Option<Self> {
        let obj = v.as_object()?;
        Some(Self {
            added: clean_tags(obj.get("added").cloned().unwrap_or(Value::Null)),
            removed: clean_tags(obj.get("removed").cloned().unwrap_or(Value::Null)),
        })
    }

    pub fn to_value(&self) -> Value {
        Value::Object(Map::from_iter([
            (
                "added".to_string(),
                Value::Array(self.added.iter().cloned().map(Value::String).collect()),
            ),
            (
                "removed".to_string(),
                Value::Array(self.removed.iter().cloned().map(Value::String).collect()),
            ),
        ]))
    }
}

/// The tags in `list` that are tags, lowercased, each once, at most `MAX_TAGS`; anything else is
/// left out.
pub fn clean_tags(list: Value) -> Vec<String> {
    let Value::Array(items) = list else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for x in items {
        let Some(s) = x.as_str() else { continue };
        let tag = s.trim().to_ascii_lowercase();
        if is_tag(&tag) && !out.contains(&tag) {
            out.push(tag);
        }
        if out.len() >= MAX_TAGS {
            break;
        }
    }
    out
}

fn is_tag(s: &str) -> bool {
    !s.is_empty() && s.len() <= 20 && s.chars().all(|c| matches!(c, 'a'..='z' | '0'..='9' | '-'))
}

/// A computer's tags: what it found, with your own added and the ones you removed taken away,
/// then capped at `MAX_TAGS` (computer-tags.md: a removed tag never takes a place from one that
/// stays, so removal has to happen before the cap, not after).
pub fn computer_tags(found: &[String], overrides: Option<&TagOverrides>) -> Vec<String> {
    let removed: Vec<String> = overrides
        .map(|o| clean_tags(Value::Array(o.removed.iter().cloned().map(Value::String).collect())))
        .unwrap_or_default();
    let added = overrides
        .map(|o| clean_tags(Value::Array(o.added.iter().cloned().map(Value::String).collect())))
        .unwrap_or_default();
    let combined: Vec<Value> = found
        .iter()
        .chain(added.iter())
        .filter(|t| !removed.contains(&t.trim().to_ascii_lowercase()))
        .cloned()
        .map(Value::String)
        .collect();
    clean_tags(Value::Array(combined))
}

/// Your changes, kept only where they change something: an added tag it didn't find, a removed
/// one it did. Used when the settings page or setup saves a typed list.
pub fn tag_overrides(found: &[String], overrides: Option<&TagOverrides>) -> TagOverrides {
    let removed = overrides
        .map(|o| clean_tags(Value::Array(o.removed.iter().cloned().map(Value::String).collect())))
        .unwrap_or_default()
        .into_iter()
        .filter(|t| found.contains(t))
        .collect::<Vec<_>>();
    let added = overrides
        .map(|o| clean_tags(Value::Array(o.added.iter().cloned().map(Value::String).collect())))
        .unwrap_or_default()
        .into_iter()
        .filter(|t| !found.contains(t) && !removed.contains(t))
        .collect();
    TagOverrides { added, removed }
}

/// What the tags are found from (computer-tags.md section 3.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineFacts {
    pub os: OsKind,
    pub arch: String,
    /// `nvidia-smi -L` answered with a GPU line.
    pub nvidia: bool,
    /// `/.dockerenv` exists.
    pub dockerenv: bool,
    /// `DISPLAY` or `WAYLAND_DISPLAY` is set.
    pub display_env: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsKind {
    Windows,
    Mac,
    Linux,
    Other,
}

/// Tags found from the facts: `windows`/`mac`/`linux`, `gpu`, `display`, `docker`.
pub fn found_tags(f: &MachineFacts) -> Vec<String> {
    let mut tags = Vec::new();
    match f.os {
        OsKind::Windows => tags.push("windows".to_string()),
        OsKind::Mac => tags.push("mac".to_string()),
        OsKind::Linux => tags.push("linux".to_string()),
        OsKind::Other => {}
    }
    if f.nvidia || (f.os == OsKind::Mac && f.arch == "aarch64") {
        tags.push("gpu".to_string());
    }
    if ((f.os == OsKind::Windows || f.os == OsKind::Mac) && !f.dockerenv) || f.display_env {
        tags.push("display".to_string());
    }
    if f.dockerenv {
        tags.push("docker".to_string());
    }
    tags
}

/// This computer's facts, looked up fresh (including an `nvidia-smi` run) on every call.
pub fn machine_facts(env: &Env) -> MachineFacts {
    let os = if cfg!(target_os = "windows") {
        OsKind::Windows
    } else if cfg!(target_os = "macos") {
        OsKind::Mac
    } else if cfg!(target_os = "linux") {
        OsKind::Linux
    } else {
        OsKind::Other
    };
    let arch = std::env::consts::ARCH.to_string();
    let nvidia = nvidia_smi_lists_a_gpu();
    let dockerenv = Path::new("/.dockerenv").exists();
    let display_env = env.get("DISPLAY").is_some() || env.get("WAYLAND_DISPLAY").is_some();
    MachineFacts {
        os,
        arch,
        nvidia,
        dockerenv,
        display_env,
    }
}

fn nvidia_smi_lists_a_gpu() -> bool {
    let Ok(out) = Command::new("nvidia-smi").arg("-L").output() else {
        return false;
    };
    out.status.success() && String::from_utf8_lossy(&out.stdout).contains("GPU")
}

/// `RETASKABLE_TAGS`: comma between, instead of the found ones and your changes. A value that
/// cleans to nothing still counts as set (an explicit empty list).
pub fn tags_from_env(env: &Env) -> Option<Vec<String>> {
    let raw = env.get("RETASKABLE_TAGS")?;
    let parts: Vec<Value> = raw.split(',').map(|s| Value::String(s.to_string())).collect();
    Some(clean_tags(Value::Array(parts)))
}

/// The tags the agent runs with: the environment's list when set, otherwise found with overrides.
pub fn effective_tags(found: &[String], overrides: Option<&TagOverrides>, env: &Env) -> Vec<String> {
    tags_from_env(env).unwrap_or_else(|| computer_tags(found, overrides))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_tags_lowercases_dedupes_and_drops_junk() {
        assert_eq!(
            clean_tags(serde_json::json!(["GPU", "gpu", "Games!", "web", ""])),
            vec!["gpu", "web"]
        );
    }

    #[test]
    fn computer_tags_adds_and_removes_over_found() {
        let found = vec!["windows".into(), "gpu".into(), "display".into()];
        let over = TagOverrides {
            added: vec!["games".into()],
            removed: vec!["gpu".into()],
        };
        assert_eq!(computer_tags(&found, Some(&over)), vec!["windows", "display", "games"]);
    }

    #[test]
    fn a_removed_tag_never_takes_a_place_from_one_that_stays() {
        // found + added together already hit MAX_TAGS once "gpu" is dropped; removing "gpu"
        // before the cap (not after) must leave room for the last added tag, "a17".
        let found = vec!["windows".into(), "gpu".into(), "display".into(), "docker".into()];
        let added: Vec<String> = (1..=20).map(|i| format!("a{i}")).collect();
        let over = TagOverrides {
            added,
            removed: vec!["gpu".into()],
        };
        let result = computer_tags(&found, Some(&over));
        assert_eq!(result.len(), MAX_TAGS);
        assert!(!result.contains(&"gpu".to_string()));
        assert!(result.contains(&"a17".to_string()));
    }

    #[test]
    fn tag_overrides_keeps_only_what_changes_the_found_list() {
        let found = vec!["windows".into(), "gpu".into()];
        let over = TagOverrides {
            added: vec!["gpu".into(), "games".into()],
            removed: vec!["gpu".into(), "missing".into()],
        };
        assert_eq!(
            tag_overrides(&found, Some(&over)),
            TagOverrides {
                added: vec!["games".into()],
                removed: vec!["gpu".into()],
            }
        );
    }

    #[test]
    fn found_tags_from_facts() {
        assert_eq!(
            found_tags(&MachineFacts {
                os: OsKind::Linux,
                arch: "x86_64".into(),
                nvidia: false,
                dockerenv: true,
                display_env: false,
            }),
            vec!["linux", "docker"]
        );
        assert_eq!(
            found_tags(&MachineFacts {
                os: OsKind::Windows,
                arch: "x86_64".into(),
                nvidia: true,
                dockerenv: false,
                display_env: false,
            }),
            vec!["windows", "gpu", "display"]
        );
        assert_eq!(
            found_tags(&MachineFacts {
                os: OsKind::Mac,
                arch: "aarch64".into(),
                nvidia: false,
                dockerenv: false,
                display_env: false,
            }),
            vec!["mac", "gpu", "display"]
        );
    }

    #[test]
    fn retaskable_tags_replaces_found_and_overrides() {
        let found = vec!["windows".into(), "gpu".into()];
        let over = TagOverrides {
            added: vec!["games".into()],
            removed: vec![],
        };
        let env = Env::from_pairs([("RETASKABLE_TAGS", "linux,web")]);
        assert_eq!(effective_tags(&found, Some(&over), &env), vec!["linux", "web"]);
        assert_eq!(
            effective_tags(&found, Some(&over), &Env::empty()),
            vec!["windows", "gpu", "games"]
        );
    }

    #[test]
    fn retaskable_tags_empty_is_none_of_the_found_ones() {
        let found = vec!["windows".into()];
        let env = Env::from_pairs([("RETASKABLE_TAGS", "not a tag!!")]);
        assert_eq!(effective_tags(&found, None, &env), Vec::<String>::new());
    }
}
