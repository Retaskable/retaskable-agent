# No release flags in agent code — Spec

Status: v0.1, draft (2026-11-11). Backlog: `remove-agent-flags`. Copies Retaskable.com's [release-flags/03.5-reading-a-flag-in-code.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/release-flags/03.5-reading-a-flag-in-code.md) (the agent bullet, item `remove-agent-flags-last`) and the TypeScript removals #3219, #3222, #3209 and #3325. Builds on [agent.md](agent.md) and [restart-and-self-update.md](restart-and-self-update.md) (slice 10, which this replaces for `own-failure-handling`). New lines are marked **[Proposal]** unless the request fixed them.

Theme: Orchestration

Kind: change

## 1. What this is

The TypeScript agent reads no release flag: every flag it once read was removed keeping its On behaviour, `agentFlags.ts` is gone, and a lint check keeps it that way. The Rust agent copies that. A new agent feature ships On, with its Off path never written; flags belong to the app and the Worker.

## 2. Decisions

- **[Decided]** Agent code has no release gates. Every flag gate is deleted, always On, or the code behind it is gone, as the TypeScript agent decided.
- **[Decided]** The four gates TypeScript removed by name, `ci-watch-fix-red`, `ci-watch-conflicts`, `rollout-batch-bisect` and `release-fewer-settings`, have no Rust counterpart: no source here reads them. Nothing is deleted for them; the lint (section 3, slice 3) keeps them out.
- **[Decided]** A lint check fails when any agent flag is read.
- **[Proposal]** Every flag the Rust agent reads today (the `on("…")`, `last("…")`, `keep_fresh("…")` and `Flags::fixed` uses under `src/`) is removed keeping its flag-On behaviour, except where a spec already says the feature is gone (`ideas-session`, the `gallery-*` flags: [agent/09.3.1-ideas.md](agent/09.3.1-ideas.md), [TRIMMED.md](TRIMMED.md)): there the code goes, not the On path. Where the On behaviour is not the one already specced, the implementer stops and asks.
- **[Proposal]** `src/flags.rs` is deleted whole, with the `RETASKABLE_FLAG_KEY` variable, `PROJECT_ID`, the `/api/flags` request and every `Flags` field, parameter and test rig left over. The Rust agent has no rollout runner, so there is no flags-as-data file to keep.

## 3. Behavior, in build order

1. **Remove the gates, a few flags per pull request** (as TypeScript did, one item each). Each slice removes the reads, the Off paths, the `Flags::fixed` setups in tests (keeping the flag-On assertions) and the "behind the `x` flag" wording in the specs' "In this Rust agent" paragraphs, rewriting them as built. Slices follow the code's groups: engines and tiers (`engine-*`, `local-*`, `weekly-*`, `model-concurrency-caps`, `tier-kept-on-resume`, `implementer-step-up`, `subscription-zero-budget`, `cursor-*`), chores and orchestrator (`chore-*`, `own-failure-handling`, `claim-next-waiting`, `triage-session`, `trial-plan-draft`, `run-limits-and-causes`, `stuck-release-drop`, `give-back-note-rules`, `release-playtests-off`), reporting and settings (`snapshot-reporting`, `watchdog-anomaly-rules`, `ci-merged-events`, `host-call-undefined-event`, `step-events-complete`, `live-sessions-reporting`, `machine-settings`, `settings-page`, `tags-reread-on-save`, `agent-ci`, `setup-check-command`, `tray-icon-menu`, `terminal-menu`, `restart-play-command`, `playtest-answer-command`, `backlog-link-route`, `ask-claude`), then the removed features' leftovers.
2. **Delete `src/flags.rs`** once nothing reads it, and its `mod` line in `lib.rs`.
3. **The lint.** A test in `cargo test` (so `cargo test` is the check, no new tool) reads every `.rs` file under `src/` (tests included), using `std::path`, and fails, one `file:line` per hit, on `Flags::`, `crate::flags`, `retaskable_agent::flags`, `.on("`-style flag reads on a `Flags` value, or `RETASKABLE_FLAG_KEY`. Its message: "Agent code has no release flags: ship the behaviour On. See CLAUDE.md 'No release flags in agent code' and docs/specs/no-agent-flags.md." The file is a pure function over (path, text) pairs, tested with a hit and a clean pair. **[Proposal]** The allowed list starts empty; a file added needs a reason in a comment.
4. **The rule is written down.** CLAUDE.md gets a rule headed "No release flags in agent code", and the README's rules list gains the same line.

## 4. Tests

The lint's own test (a hit per pattern, a clean file, the message). The existing tests keep their flag-On assertions and lose the rigs that set flags.

## 5. Acceptance

- No file under `src/` mentions `Flags`, `crate::flags` or `RETASKABLE_FLAG_KEY`, and `src/flags.rs` is gone.
- Adding `Flags::from_env()` anywhere under `src/` fails `cargo test` with the message above.
- No spec here says "behind the … flag" for the agent.
- `cargo fmt`, `cargo clippy --all-targets -- -D warnings` and `cargo test` pass.
