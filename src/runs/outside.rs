//! Whether a failed run's why names a cause that isn't the work's (docs/specs/pieces.md section 3.6, "Outside
//! causes"). Plain code on the words, never a model's call: the store gives such a piece a look at once.
use std::sync::LazyLock;

use regex::Regex;

use crate::engines::engine::signed_out_text;

static OUTSIDE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)unfilled \{\{|is missing the \{\{[^}]*\}\} variable|spawn_error",
        r"|starting .+: .*\(os error \d+\)",
        r"|found claude signed out|this computer can't run .+ sessions yet",
        r"|\bEACCES\b|\bEPERM\b|permission denied|access is denied",
        r"|\bECONNREFUSED\b|\bENOTFOUND\b|\bETIMEDOUT\b|could not resolve host",
    ))
    .expect("outside-cause pattern")
});

/// Whether a `failed` run's why is a cause outside the work: an unfilled prompt variable, the engine failing to
/// start or sign in, a permission denied, or the code host unreachable.
pub fn is_outside(why: &str) -> bool {
    OUTSIDE.is_match(why) || signed_out_text(Some(why))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn causes_that_are_not_the_works_are_outside() {
        for why in [
            "the planner prompt has an unfilled {{…}} placeholder",
            "prompt is missing the {{item}} variable",
            "spawn_error: no such file",
            "starting /usr/bin/claude: No such file or directory (os error 2)",
            "found Claude signed out on this computer (Not logged in)",
            "this computer can't run cursor sessions yet",
            "Not logged in · Please run /login",
            "EACCES: open '.git/index.lock'",
            "EPERM: operation not permitted",
            "fatal: Permission denied",
            "Access is denied.",
            "connect ECONNREFUSED 127.0.0.1:443",
            "getaddrinfo ENOTFOUND github.com",
            "ETIMEDOUT",
            "fatal: Could not resolve host: github.com",
        ] {
            assert!(is_outside(why), "{why}");
        }
    }

    #[test]
    fn ordinary_failures_are_not() {
        for why in [
            "checks still fail: test",
            "Implementer gave up: the spec is unclear",
            "the reviewer's requests are still open: 2",
        ] {
            assert!(!is_outside(why), "{why}");
        }
    }
}
