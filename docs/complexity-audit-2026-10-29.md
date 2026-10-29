# Complexity audit — 2026-10-29

A snapshot of where the Rust agent's complexity sits at `f5406be`, and what would lower it. Numbers
are from `clippy`'s complexity lints, a scratch build that made every module private to find what
`main` can't reach, and a module-graph script; the method is at the end so it can be re-run.

## Summary

The code itself is in good shape: small functions, almost no `unwrap` in production paths, no
`unsafe`, no `TODO`s, and more test code than production code. The complexity that matters is
structural:

1. **About 40% of the production code isn't reachable from the binary yet.** It's ported and
   tested, but nothing runs it. This is the port plan working as intended (see `src/lib.rs`), but
   it's the single biggest carrying cost, and the orchestrator loop that would connect it is where
   most of the remaining risk lives.
2. **Four real module cycles**, and one helper (`render_prompt`) living in the wrong module, which
   makes `release_notes` a dependency of six unrelated features.
3. **`main.rs` does too much wiring by hand**: `serve` is 210 lines and has the highest
   cognitive complexity in the crate.
4. **Two stringly-typed seams**: telemetry events (`kind: &str` + a JSON `Map`, read back with
   `.get("…")`) and the env lookup closure, which is threaded through 46 signatures with four
   different bounds.

Nothing here is urgent. Items 2–4 are cheap, and are best done before the orchestrator loop starts
pulling the unwired modules in.

## Size

| | Lines |
|---|---:|
| All `.rs` under `src/` (123 files) | 38,361 |
| Production (test files and inline `mod tests` removed; 101 files) | 21,202 |
| Tests | ~17,200 |
| Comment lines in production code | 2,946 (14%) |

Largest production modules: `engines` 2,466 · `release_notes` 1,684 · `telemetry` 1,520 ·
`implementer` 1,390 · `triage` 1,056 · `config` 1,027 · `settings_server` 1,000.

## Function-level complexity

905 production functions. Median length is 9 lines, p90 is 33 and p99 is 81. Only 10 functions
are over 80 lines. `clippy` with tightened thresholds (`too_many_lines` 80, `cognitive_complexity`
20, `too_many_arguments` 6, `excessive_nesting` 5) flags 27 sites in total:

| Function | Lines | Cognitive | Notes |
|---|---:|---:|---|
| `main.rs` `serve` | 210 | 39 | Builds the view, lane-size pollers, the exit/restart closure, connectors, auto-link, setup check, updater, Linux login items and the HTTP server, all inline |
| `main.rs` `main` | 178 | — | Argument dispatch, config load, port binding |
| `main.rs` `start_project` | 74 | 49 | Four independent per-project startups in one function |
| `main.rs` `link_command` | — | 34 | |
| `link/mod.rs` `DeviceTokens::refresh` | ~65 | 49 | Token exchange, rotation, `invalid_grant` handling and logging in one body |
| `release_notes/saved.rs` `ready` | — | 36 | |
| `phone.rs` `watch` | — | 27 | |
| `telemetry/report/describe.rs` `describe` | 154 | — | One big `match kind { "…" => … }` over JSON (see *Stringly-typed telemetry*) |
| `bin/bundle.rs` `run` | 131 | — | Build-tool binary, lower priority |
| `implementer/code_gates.rs` `duplicates` | 115 | — | |
| `release_notes/group.rs` `group` | 107 | — | |
| `telemetry/report/agent_report.rs` `lane_section` | 102 | — | |
| `settings_server/mod.rs` `post_settings` | 90 | — | |

Too many arguments (7): `Connector::start`, `galleries.rs:346`, `ideas/profiles.rs:23`,
`phone.rs:132`, `release_notes/gather.rs:115`. Excessive nesting: `engines/json_result.rs:90`,
`store/machine.rs:84`, and the restart closure in `serve`. `triage/schedule.rs:49` (`NeedsHumanFacts`)
has more than three `bool`s.

Three of the five highest cognitive-complexity scores are in `main.rs`. Everything else is local and
reads fine.

## 1. Unwired code (the biggest item)

The scratch build made every module private and turned `main` into a library function. It reports
**594 items that nothing reachable from `main` uses**. That's roughly **8,300 production lines**
(estimated from the item spans), across these modules:

| Module | ≈ unreached lines | | Module | ≈ unreached lines |
|---|---:|---|---|---:|
| `engines` | 1,544 | | `smoke` | 197 |
| `release_notes` | 1,288 | | `reviewer` | 189 |
| `implementer` | 1,186 | | `playtest_groups` | 188 |
| `triage` | 881 | | `scan` | 181 |
| `ideas` | 514 | | `backlog` | 179 |
| `ci_coverage` | 341 | | `item_lifecycle` | 147 |
| `repo` | 311 | | `checks` | 143 |
| `scouts` | 311 | | others | < 115 each |

`implementer`, `ci_coverage`, `scouts`, `repo`, `reviewer`, `scan`, `checks` and `review_context`
aren't reached at all. `engines` is only reached for `engine_for` and Claude path resolution;
`run_session`, the stream readers and transcripts aren't. `orchestrator::tick` and
`backlog::Client` (`claim_next` and the rest) aren't called either.

This is the documented porting strategy, not rot. But it has two costs:

- **Integration risk is deferred, not removed.** Each module was designed against its own unit
  tests. The first caller (the orchestrator loop) is where mismatched assumptions will show up:
  sync vs. async, who owns a `Store`, how errors propagate. Every module ported before that loop
  exists adds to the work of reconciling them.
- **`pub` everywhere hides real dead code.** Because `lib.rs` exports every module, `rustc` can't
  warn about anything. The scratch build also found 34 unused re-exports (e.g. in
  `engines/mod.rs`, `ideas/mod.rs`, `config/mod.rs`), which would normally be flagged.

**Recommendation:** make wiring the orchestrator loop (`orchestrator::tick` → `backlog::Client` →
`implementer` → `checks`/`scouts`/`reviewer` → `item_lifecycle`) the next priority, ahead of
porting more leaf modules. Until then, consider changing modules that aren't wired to `pub(crate)`
with a crate-level `#![allow(dead_code)]` scoped to them, or a CI job that runs the scratch build,
so that *newly* dead code shows up in review.

## 2. Module structure

### Real cycles

| Cycle | Edge that closes it |
|---|---|
| `config` → `store` → `smoke` → `config` | `config/mod.rs:78` calls `store::write_json_atomic`; `store/project.rs:14` uses `smoke::SmokeState` |
| `engines` ↔ `setup_check` | `engines/cursor/mod.rs:133` calls `setup_check::resolve_cursor`; `setup_check` calls `engines::resolve_claude_on_path` |
| `api` ↔ `settings_server` | `settings_server/mod.rs:21` uses `api::not_found_response` |
| `commands` ↔ `connector` | `commands.rs:21` uses `connector::{AgentView, Exit}` |

Each one has a simple fix:

- Move `write_json_atomic` into a small leaf module (`fs_util` or `paths`). Move `SmokeState` and
  `SmokeStatus` next to the other persisted state in `store`, or into a shared `model` module.
- Move binary resolution (`resolve_claude*`, `resolve_cursor*`, `resolve_pi*`) into one leaf
  module, `engines::resolve`, that `setup_check` depends on, and not the other way round.
- Move `not_found_response` into `settings_server`, or into a tiny `http_util`.
- Move `AgentView`, `ProjectView` and `Exit` out of `connector` into a `view` module that both
  depend on.

The script also reported a 6-module cycle (`backlog`/`ideas`/`playtest_groups`/`release_notes`/
`reopen`/`triage`), plus `reviewer`↔`scouts` and `release_install`↔`release_update`. Those come
only from intra-doc links (`[`crate::…`]`), not code, so they aren't real dependencies.

### A shared helper in the wrong place

`release_notes::render_prompt` and `release_notes::project_words` are the crate's general prompt
templating. Six features import them: `ideas`, `implementer`, `reviewer`, `scouts`, `triage` and
`playtest_groups`. So every one of those features depends on release notes. Move both into a
`prompt` module (it could also own `prompts/*.md` loading).

### Low-cohesion modules

- **`lanes.rs`** (621 production lines) mixes three jobs: git worktree management, a
  Cargo-manifest parser for out-of-repo path dependencies (`outside_paths`, `path_eq_values`,
  `quoted_string`, …), and cross-OS process discovery and killing (`processes_in`,
  `list_processes`, `kill_tree`, `descendants_of`). Splitting it into `lanes`, `lanes::manifest`
  and `proc::tree` would make each one testable on its own.
- **`SettingsServer`** has one 440-line `impl` covering session auth, CSRF, rate limiting, host
  checks, reading settings, and eight validate/save pairs. Moving the validate/save pairs to
  `settings_server/save.rs` and auth/rate-limit to `settings_server/auth.rs` would follow the
  module's existing `tests.rs` split.

## 3. `main.rs` wiring

`serve` hand-builds about ten long-lived tasks and shares state between them with
`Arc<Mutex<Option<Arc<ReleaseUpdater>>>>` (the updater slot), so the restart closure can see an
updater that's created later in the same function. The closure itself spawns a thread, sleeps
200 ms, locks, possibly installs, possibly relaunches, then exits.

**Recommendation:** extract `fn restart_handler(…) -> Exit`, `fn spawn_lane_size_pollers(…)`,
`fn spawn_release_updater(…)` and `#[cfg(target_os = "linux")] fn register_desktop_entries(…)`.
Building the updater *before* the exit closure would replace the `Mutex<Option<…>>` with a plain
`Option<Arc<…>>` captured by value. That alone should bring `serve` under 80 lines.
`start_project` splits the same way (state trim, lane pool, phone watch, engine log). The
orchestrator loop will need a home for these tasks anyway, so an `Agent` runtime struct that owns
them would avoid re-wiring `main` later.

## 4. Stringly-typed seams

### Telemetry events

`EventLog::add(kind: &str, fields: Map<String, Value>)` is how events are written. The reports
(`describe.rs`, `why.rs`, `agent_report.rs`) read them back with about 60 `.get("…")`/`json!` sites and a
`match` on kind strings. The event schema exists only as matching string literals on both sides.
A typo in either one compiles and fails silently. This will get worse once the orchestrator starts
emitting events.

**Recommendation:** add `#[derive(Serialize, Deserialize)] #[serde(tag = "kind")] enum Event`
with one variant per kind, writing and reading through it. `describe` then becomes an exhaustive
`match`, and the compiler finds kinds the report forgets. The on-disk format can stay the same.
(JSON `Value` handling in `engines/*` is fine: it parses the output of external CLIs, which has no
fixed schema.)

### The env closure

There are 46 signatures that take an env lookup. 39 use `impl Fn(&str) -> Option<String>`, 5 add
`+ Copy`, 2 add `Send + Sync + 'static`, and `settings_server` boxes it as `EnvFn`. The varying
bounds spread from `main` to its callees. A `pub type`/trait `Env` (or a cheap `Clone` struct
wrapping `Arc<dyn Fn…>`, with a `from_process()` constructor and a `from_map()` for tests) would
make signatures shorter and uniform without losing testability.

## Smaller observations

- **`gh` calls in `repo.rs`** repeat the `Command::new("gh")…output().ok().filter(success)` →
  parse pattern 7 times, with no timeout. A `gh_json::<T>(args, cwd)` helper next to
  `git_bin::command` would remove the repetition. `proc::run_command_with_timeout` already exists
  for the timeout. (`repo` isn't wired yet, so now is the cheap time to do this.)
- **Git starts consistently through `git_bin::command`** in production code. The bare
  `Command::new("git")` calls are all in tests or `bin/bundle.rs`. Good.
- **One `reqwest::blocking` user** (`smoke.rs`). Everything else uses the async client. Once
  `smoke` is called from the async orchestrator it'll need `spawn_blocking`, or a switch to the
  async client.
- **Error handling is disciplined:** 67 `unwrap`s and 18 `expect`s in about 21k production lines,
  mostly on mutex locks and the clock. There are 3 `#[allow]`s and no `unsafe`.

## How these numbers were made

```sh
# Function-level lints (thresholds in a scratch clippy.toml):
#   too-many-lines-threshold = 80, cognitive-complexity-threshold = 20,
#   too-many-arguments-threshold = 6, excessive-nesting-threshold = 5
CLIPPY_CONF_DIR=<dir> cargo clippy --lib --bins -- -A clippy::all \
  -W clippy::too_many_lines -W clippy::cognitive_complexity -W clippy::too_many_arguments \
  -W clippy::excessive_nesting -W clippy::type_complexity -W clippy::struct_excessive_bools

# Reachability: in a scratch copy, `sed -i 's/^pub mod /mod /' src/lib.rs`, move main.rs into the
# lib as `pub mod bin_main` (with `retaskable_agent::` → `crate::`, and `main` → `pub async fn entry`),
# then `cargo build --lib` and count the dead_code warnings.
```

The line counts strip test files (`tests.rs`, `*_tests.rs`, `test_support`) and inline
`#[cfg(test)] mod tests { … }` blocks. Module edges are `crate::<module>` references in
production code. The "≈ unreached lines" column brace-matches from each dead item to its end,
including its doc comment, so treat it as an estimate.
