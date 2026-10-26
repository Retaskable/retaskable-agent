//! On-disk state under `.retaskable/`: each project's `projects/<id>/state.json`, and this
//! computer's `machine.json` (agent-settings.md section 3; TypeScript's `MachineStore`).
mod machine;
mod project;

pub use machine::MachineStore;
pub use project::Store;

use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;

/// `writeJsonAtomic(file, data, compact)`: compact (`JSON.stringify` with no whitespace) when
/// `compact`, indented otherwise. The files people may read or edit stay indented; only
/// `state.json` and `machine.json` are written compact.
pub(crate) fn write_json_atomic(file: &Path, data: &Value, compact: bool) -> Result<()> {
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
