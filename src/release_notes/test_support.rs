//! Shared fixtures for this module's own tests: an [`Item`]/[`MergedPr`] builder, the "Charts"
//! item+PR pair several tests across `group` and `render` build identically, a fixed [`Range`], and
//! a fake `gh`/`git` binary that just prints a fixture file.
use std::path::Path;

use crate::backlog::{Item, ItemStatus};

use super::model::{MergedPr, Range};

pub(crate) fn item(
    id: &str,
    title: &str,
    status: ItemStatus,
    pr_url: Option<&str>,
    spec_refs: &[&str],
    milestone: Option<&str>,
) -> Item {
    Item {
        id: id.to_string(),
        title: title.to_string(),
        status,
        rank: 0,
        do_next: false,
        depends_on: Vec::new(),
        spec_refs: spec_refs.iter().map(|s| s.to_string()).collect(),
        slice: 0,
        remaining: None,
        pr_url: pr_url.map(|s| s.to_string()),
        branch: None,
        kind: None,
        milestone: milestone.map(|s| s.to_string()),
    }
}

pub(crate) fn pr(number: u64, title: &str, url: &str, head_ref_name: &str, merged_at: &str) -> MergedPr {
    MergedPr {
        number,
        title: title.to_string(),
        url: url.to_string(),
        body: String::new(),
        merged_at: merged_at.to_string(),
        head_ref_name: head_ref_name.to_string(),
        merge_commit_oid: None,
    }
}

pub(crate) fn range() -> Range {
    Range {
        from: "2026-09-23T00:00:00Z".to_string(),
        to: "2026-09-30T00:00:00Z".to_string(),
        generated_at: "2026-09-30T12:00:00Z".to_string(),
    }
}

/// The "Charts" item and its merged PR, built identically by several tests in `group` and `render`.
pub(crate) fn charts_item_and_pr() -> (Item, MergedPr) {
    let charts_item = item("1", "Charts", ItemStatus::OnMain, None, &["docs/specs/charts.md"], None);
    let charts_pr = pr(
        1,
        "Add charts",
        "https://github.com/x/y/pull/1",
        "retaskable/1-charts",
        "2026-09-24T00:00:00Z",
    );
    (charts_item, charts_pr)
}

pub(crate) fn write_fake_binary_printing(path: &Path, fixture: &Path) {
    if cfg!(windows) {
        std::fs::write(path, format!("@type \"{}\"\r\n", fixture.display())).unwrap();
    } else {
        std::fs::write(path, format!("#!/bin/sh\ncat \"{}\"\n", fixture.display())).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
}
