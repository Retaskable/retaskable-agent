//! docs/specs/agent-ci.md section 3.3: the runner's GETs go through the neutral `github_get` (ETags, so an unchanged
//! answer is a free 304, and the account's allowance), a port of Retaskable.com's `.agent-ci/etagCache.mjs`.
use anyhow::Result;

use super::report::Host;

impl Host {
    /// `gh api <endpoint>` as a GET with the last answer's ETag, so an unchanged answer is a free 304.
    pub fn get_cached(&self, endpoint: &str) -> Result<String> {
        crate::github_get::get_cached_in(self.program.as_os_str(), &self.dir, &self.repo, endpoint)
    }
}
