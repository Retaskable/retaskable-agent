#!/usr/bin/env bash
# What Retaskable's agent-ci runner runs for this repo (Retaskable.com's .agent-ci/runner.mjs): the same checks as
# .github/workflows/ci.yml, on Linux. Usage: bash .agent-ci/checks.sh <pull_request|push>
set -ex
rustup component add clippy rustfmt
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
