//! Whether an item's change merged while the item was away (`landedWhileAway` in the TypeScript agent's `merge.ts`):
//! parked, waiting on a person, cut by a restart, or merged by a person or another tool on the code host. Read before
//! a session is launched for the item, so a run doesn't pick up a branch the host has since merged and deleted.
use std::path::Path;

use anyhow::{Context, Result};

/// `Some(merge commit)` when change `pr` is merged and its head is `branch`, else `None`. The head is checked so a
/// record that still names an earlier slice's merged change doesn't land the next slice.
pub fn merged_commit(gh: &Path, repo: &Path, pr: u64, branch: &str) -> Result<Option<Option<String>>> {
    let number = pr.to_string();
    let args = ["pr", "view", &number, "--json", "state,headRefName,mergeCommit"];
    crate::host_call::guard("gh", repo, &args)?;
    let mut cmd = crate::spawn::command(gh);
    cmd.current_dir(repo).args(args);
    let out = crate::implementer::publish::run(cmd, None, &format!("gh pr view {pr}"))?;
    #[derive(serde::Deserialize, Default)]
    #[serde(default)]
    struct Commit {
        oid: Option<String>,
    }
    #[derive(serde::Deserialize, Default)]
    #[serde(default, rename_all = "camelCase")]
    struct View {
        state: String,
        head_ref_name: String,
        merge_commit: Option<Commit>,
    }
    let view: View = serde_json::from_str(out.trim()).context("reading gh pr view's answer")?;
    if !view.state.eq_ignore_ascii_case("MERGED") || view.head_ref_name != branch {
        return Ok(None);
    }
    let oid = view.merge_commit.and_then(|c| c.oid).filter(|o| !o.is_empty());
    Ok(Some(oid))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn gh_saying(dir: &Path, json: &str) -> std::path::PathBuf {
        let path = dir.join("fake_gh.sh");
        crate::test_support::write_executable(&path, &format!("#!/bin/sh\necho '{json}'\n"));
        path
    }

    #[test]
    fn a_merged_change_on_the_items_branch_has_landed_with_its_commit() {
        let dir = tempfile::tempdir().unwrap();
        let gh = gh_saying(
            dir.path(),
            r#"{"state":"MERGED","headRefName":"retaskable/a-s0","mergeCommit":{"oid":"abc123"}}"#,
        );
        let got = merged_commit(&gh, dir.path(), 9, "retaskable/a-s0").unwrap();
        assert_eq!(got, Some(Some("abc123".to_string())));
    }

    #[test]
    fn an_open_change_or_another_slices_has_not() {
        let dir = tempfile::tempdir().unwrap();
        let open = gh_saying(dir.path(), r#"{"state":"OPEN","headRefName":"retaskable/a-s0"}"#);
        assert_eq!(merged_commit(&open, dir.path(), 9, "retaskable/a-s0").unwrap(), None);
        let other = gh_saying(
            dir.path(),
            r#"{"state":"MERGED","headRefName":"retaskable/a-s0","mergeCommit":{"oid":"x"}}"#,
        );
        assert_eq!(merged_commit(&other, dir.path(), 9, "retaskable/a-s1").unwrap(), None);
    }
}
