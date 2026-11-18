//! Lighthouse on this computer (docs/specs/daily-lighthouse.md): the run itself ([`run`]) is the `lighthouse` job's
//! (`jobs::lighthouse`); the store decides the day, reads the raw result and writes up what is new. What stays here is
//! the tag a Lighthouse write-up's draft request starts with, so the drafter runs it on the chore tier.

pub mod run;

/// The tag a Lighthouse write-up's request starts with.
pub const TAG: &str = "[Lighthouse]";

/// A draft is a Lighthouse write-up when its request starts with [`TAG`]; the drafter runs it on the chore tier.
pub fn is_lighthouse_request(request: &str) -> bool {
    request.trim_start().starts_with(TAG)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_is_a_lighthouse_write_up_by_its_tag() {
        assert!(is_lighthouse_request("  [Lighthouse] Fix what Lighthouse found"));
        assert!(!is_lighthouse_request("Fix the Lighthouse score"));
    }
}
