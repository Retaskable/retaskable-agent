//! Small file-system helpers shared across modules; a leaf (it depends on no other module here).
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;

/// `writeJsonAtomic(file, data, compact)`: compact (`JSON.stringify` with no whitespace) when
/// `compact`, indented otherwise. The files people may read or edit stay indented; only
/// `state.json` and `machine.json` are written compact.
pub fn write_json_atomic(file: &Path, data: &Value, compact: bool) -> Result<()> {
    let text = if compact {
        serde_json::to_string(data)?
    } else {
        serde_json::to_string_pretty(data)?
    };
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let tmp_name = format!(
        "{}.tmp",
        file.file_name().context("json file has no name")?.to_string_lossy()
    );
    let tmp = file.with_file_name(tmp_name);
    std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, file).with_context(|| format!("renaming {} to {}", tmp.display(), file.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_missing_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("nested").join("deeper").join("state.json");
        write_json_atomic(&file, &serde_json::json!({"a": 1}), true).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), r#"{"a":1}"#);
    }

    #[test]
    fn compact_has_no_whitespace_and_indented_is_pretty_printed() {
        let dir = tempfile::tempdir().unwrap();
        let data = serde_json::json!({"a": 1});

        let compact = dir.path().join("compact.json");
        write_json_atomic(&compact, &data, true).unwrap();
        assert_eq!(std::fs::read_to_string(&compact).unwrap(), r#"{"a":1}"#);

        let indented = dir.path().join("indented.json");
        write_json_atomic(&indented, &data, false).unwrap();
        assert_eq!(std::fs::read_to_string(&indented).unwrap(), "{\n  \"a\": 1\n}");
    }

    #[test]
    fn overwrites_an_existing_file_and_leaves_no_tmp_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("state.json");
        std::fs::write(&file, "old").unwrap();

        write_json_atomic(&file, &serde_json::json!({"b": 2}), true).unwrap();

        assert_eq!(std::fs::read_to_string(&file).unwrap(), r#"{"b":2}"#);
        assert!(!dir.path().join("state.json.tmp").exists());
        // Nothing else was left behind in the directory.
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("state.json")]);
    }
}
