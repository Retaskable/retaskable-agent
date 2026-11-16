//! A failure of the machine, not the code (`ciInfra.ts` in the TS agent): a failed check whose log shows a known
//! signature of the runner or its tools. Plain pattern matching, no model. A fix session sent for one finds nothing to
//! change, makes no commit and fails, so a piece's `watch` run runs CI again once instead of ending red.
use std::sync::LazyLock;

use regex::RegexSet;

static INFRA: LazyLock<RegexSet> = LazyLock::new(|| {
    RegexSet::new([
        r"panic: runtime error",
        r"fatal error: fault",
        r"found pointer to free object",
        r"The service was stopped",
        r"Segmentation fault",
        r"exit code 139",
        r"TAR_ENTRY_INVALID",
        r"(?i)integrity checksum failed",
        r"EINTEGRITY",
        r"(?i)Unable to find the symbol offset",
        r"spawn gh ENOENT",
        r"(?i)Cannot connect to the Docker daemon",
        r"(?i)error during connect",
        r"(?i)docker: Error response from daemon",
    ])
    .expect("the machine signatures are valid patterns")
});

/// True when there are logs and every one shows a machine signature. A log that couldn't be read (empty) or any one
/// without a signature is the change's own, as before.
pub fn all_infra<'a>(logs: impl IntoIterator<Item = &'a str>) -> bool {
    let mut any = false;
    logs.into_iter().all(|log| {
        any = true;
        INFRA.is_match(log)
    }) && any
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_log_needs_a_signature() {
        assert!(all_infra(["Error: spawn gh ENOENT", "Segmentation fault (core dumped)"]));
        assert!(!all_infra(["Error: spawn gh ENOENT", "expected 1 to equal 2"]));
        assert!(!all_infra(["Error: spawn gh ENOENT", ""]));
        assert!(!all_infra(Vec::<&str>::new()));
    }
}
