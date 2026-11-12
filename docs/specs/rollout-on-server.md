# Rollout on the server — Spec

Status: v0.1, draft (2026-11-11). Backlog: `rollout-on-server`, `rollout-production-only`, `rollout-cloudflare-env`. Copies Retaskable.com's [rollout-on-server.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/rollout-on-server.md) (sections 4, 5 and 7, the agent's part only, with the later changes of 2026-11-09 to 11-10) and the TypeScript changes #3401, #3444, #3457, #3459 and #3463 to #3475 (`agent/src/rolloutRun.ts`, `rolloutDeps.ts`, `rolloutReport.test.ts`). Builds on [agent/08-http-api.md](agent/08-http-api.md) (the agent keeps no rollout of its own, catch-up piece P8), [merge-queue-from-store.md](merge-queue-from-store.md) (the same shape: the store decides, the computer does what it is told), [docker.md](docker.md) and [agent/06.3-review.md](agent/06.3-review.md) (the staging smoke).

Theme: Orchestration

Kind: new

## 1. What it is

The computer reports and obeys; the store's machine decides. A release is rolled out by the store: which commit goes next, whether to promote it, each step of the traffic, how long to hold, when the numbers say stop and when to roll back. The computer has no step, hold or stop rule, and remembers nothing about a rollout across a restart. It does three things, in a new `src/rollout/` module:

1. **Report** what only git can say: production's commit, and the commits staging has that production lacks, each with the store items it holds and whether staging serves it.
2. **Poll** the store in a loop: send the last result, get back one instruction or "idle".
3. **Act**: run exactly that instruction (`promote`, `traffic <percent>` or `health`) with no model session, and send what came of it.

A computer that can't reach the store runs nothing and sets no traffic (fail closed, no fallback). Rolling out is the store's (the runner lease, the two-computer rule, the stop and resume commands, the seed from the old `rollout` fact); none of that is built here.

## 2. Decisions

- **[Decided]** The computer takes only the three instructions and runs only what the store told it. Claude usage never gates a release (no session is involved).
- **[Decided]** Only one instruction is out at a time, with an id. A repeated id is answered with the result already kept, never run again. A command that outlives the instruction's `timeoutMs` (5 minutes when absent or bad) is a failed result.
- **[Decided]** `health` is never run more than once every 10 seconds (`HEALTH_MIN_MS`), whatever the store asks. A too-early instruction is not run: the computer waits out the rest and polls again. This guards Cloudflare's API.
- **[Decided]** retaskable.com's own project is a project like any other: its `traffic` and `health` are commands in its settings (`npm run rollout:traffic -w cloud --` and `npm run rollout:health -w cloud`), run by a computer with `CLOUDFLARE_ACCOUNT_ID` and `CLOUDFLARE_API_TOKEN` in its environment. A saved `site: true` in the setting is read and dropped: it claims no rollout and runs nothing.
- **[Decided]** A project that merges straight to production (no staging branch) rolls out the same way; it only reports differently (section 3, slice 2).
- **[Decided]** Each report, each change in what the store's poll answers, and each result are logged once per change, so a poll every few seconds says each thing once.
- **[Proposal]** `src/rollout/` holds `mod.rs` (`cycle` and `step`), `report.rs` (the ready report), `deps.rs` (a trait over the store ops, the command runner, git, `gh api` and the staging and production sites, so the tests use fakes, as `src/backlog/runs/tests.rs` does; the clock is tokio's, paused in tests) and `live.rs` (one task per project, started from `main`). The report is rebuilt at most once a minute and sent when it differs from the last one sent. A failed command's `why` and `health`'s `line` are read from the command's combined output.
- **[Proposal]** The gate for taking the runner role is the computer's own: this computer may release the project (not paused here or in the store, the `traffic` and `health` commands approved on the site, `src/rollout/live.rs`). A project's task starts when its commands, as approved now, make the rollout apply, so commands that arrive after startup need no restart. A code host that can't be reached shows as a store error on the poll and nothing runs. It never decides anything about a rollout. A computer that doesn't release sends no report and does not poll.
- **[Proposal]** The Rust agent has no `promote` checks yet (the TypeScript `promote.ts`). Until a later item ports them, a `promote` instruction for a project with a staging branch is answered `{ ok: false, why: "this computer can't promote yet" }`, never run in some lesser way. Under Production only a `promote` succeeds and does nothing (slice 2).
- **[Proposal]** The store's rules below are written down so the computer's logs and tests make sense; they are not built in the agent: never releasing a commit production already has, an old waiting try-out not holding every release, the deploy phase accepting a newer uploaded version, and the seed past a crashed old-path `running` fact after an hour (`STALE_OLD_ROLLOUT_MS`). They live in the store (Retaskable.com `rollout-on-server.md` sections 5.4 and 6.4).

## 3. Behavior, in build order

### Slice 1: report, poll, act (`rollout-on-server`)

1. **Whether it applies** (`src/rollout/mod.rs`, `uses_store`): the project's `deploy.production.rollout` has both `traffic` and `health` set, and the project has a staging branch or a production branch. A `site` flag is ignored. Anything else: no report, no poll, one log line saying which.
2. **The report** (`rollout_ready { production, commits }`, an agent op through `Client::op`), sent after each fetch of the project's branches and when it changes. `production` is the sha of `origin/<production branch>`; `commits` are `git log --first-parent --format=%H %ct production..origin/<staging branch>`, newest first, at most 200, each `{ sha, at (ms), items, served }`. `items` are the ids of store items whose `mergeCommit` that commit contains and production doesn't, among those whose status is `on_branch`, `on_main` or `in_production` (or reopened); `served` is whether the commit is the one staging serves or an ancestor of it (`git merge-base --is-ancestor`). What staging serves is, with a smoke, the sha of the last smoke that passed (`State.smoke`); without one, the sha its `versionPath` answers; without an address, `origin/<staging branch>`. Nothing is sent when a branch, the items or the log can't be read; the computer logs why. A project with no staging branch reports as slice 2 says.
3. **The loop**: while the project applies and the computer releases, `rollout_poll { machine, result? }` and wait:
   - `{ held: false }`: another computer runs it. Drop the kept result, run nothing, poll again in 5 seconds.
   - `{ do, afterMs }`: run `do` (step 4) and poll again at once with its result, unless the answer was the same instruction id as the kept result: then wait `afterMs` (5 seconds when 0).
   - `{ idle, why?, afterMs }`: wait `afterMs` (5 seconds when absent). The log line carries `why`.
   - The store can't be reached: log it, keep the result for the next poll, wait 5 seconds, run nothing.
   The answer's `epoch` is kept only to beat with.
4. **Act** (`do: { id, command, sha, percent?, timeoutMs, by? }`):
   - A `sha` that isn't 40 hex digits fails with "that isn't a commit".
   - `traffic`: needs the `traffic` command and a whole `percent` from 0 to 100, else fails with "no traffic command, or a bad percent". Runs `<traffic> <percent>` through the shell in the project's checkout; ok is exit 0.
   - `health`: runs the `health` command; ok is exit 0 and `line` is the last non-empty line of stdout, at most 2048 characters. Rate limited as in section 2.
   - `promote`: as section 2.
   - Anything else fails with "unknown instruction <name>".
   - A failure's `why` is the last non-empty line of stderr (else stdout), or `exit <code>`, cut to its last 300 characters.
   - While a command runs longer than 10 seconds the computer sends `rollout_beat { epoch }` every 15 seconds. If a beat answers `held: false` the result is dropped, with a log line, and the next poll goes out in 5 seconds with nothing to say.
5. **Logging**, through the project's log, once per change of each key: why nothing was reported or what was (production's first 9 characters, commits waiting, how many served, how many items); what the poll answered (`told to traffic 25 <sha9>`, `idle: <why>`, `another computer runs the rollout`, a poll error); each result (`<command> <sha9>: ok`, `failed, <why>` or `no longer the runner, result dropped`); and `health asked again too soon, waits Ns`.
6. **Restart**: nothing is kept on disk. A computer that starts polls and does only what the store says; it never resumes an instruction itself.
7. **No second path**: the Rust agent keeps no step index, hold clock, stop rule, resume logic or `rollout` fact (a test greps `src/` for them), and the old `release_*` messages are not sent.

### Slice 2: Production only (`rollout-production-only`)

8. **The report** for a project with no staging branch: `production` is the commit its `versionPath` serves (nothing is reported while it can't be read); `commits` are the base branch's first-parent commits since it. `served` is true up to the newest of the last 10 first-parent commits whose checks all passed (Workers Builds' upload is one of them) and false after. A commit whose checks haven't posted yet is skipped for the next older one, and one with none serves nothing.
9. **Reading CI**: the checks come from the commit's check-runs. Only when the token can't read check-runs does the computer read the commit's combined statuses instead (`gh api repos/<repo>/commits/<sha>/status`); the failure to read check-runs is the only reason to try it. A commit still pending is not served.
10. **`promote`** does nothing and succeeds: the merge is the release and its build is uploaded already. The store then issues `traffic` and `health` as for a staging project.
11. **The smoke**: nothing under Production only runs one, so a leftover `State.smoke` is not read for `served`.
12. **The idle log** says why, with what the store sent: no report yet, a stale report, no rollout commands in the store's settings, a stop (with its reason), a refusal, or the counts reported / served / releasable.

### Slice 3: Cloudflare in the environment (`rollout-cloudflare-env`)

13. **Docker** ([docker.md](docker.md) section 3.3): `docker/agent/compose.yaml` passes `CLOUDFLARE_ACCOUNT_ID` and `CLOUDFLARE_API_TOKEN` through (`${NAME:-}`, empty when unset), and `docker/agent/README.md` lists them as optional: the computer that runs retaskable.com's rollouts. The commands read them from their environment; the agent never reads them itself and logs neither.

## 4. Tests

Fakes for the store op, the clock, git and the command runner. A first poll with nothing to do idles and logs why. `promote`, `traffic 25` and `health` run once with the result sent on the next poll; a repeated id is not run again; a bad sha and a bad percent fail; a command past `timeoutMs` fails. A second `health` inside 10 seconds is not run and waits out the rest. A beat answering `held: false` drops the result. A store that errors runs nothing. `held: false` runs nothing. A `site` setting alone does not apply. Under Production only: `served` stops at the newest passing commit, a pending commit is skipped, the statuses are read only when check-runs can't be, `promote` succeeds with no command, a leftover smoke is ignored. The report cuts at 200 commits and is not sent when git can't be read. Logs are written once per change.

## 5. Acceptance

- A project with both commands set and a staging or production branch reports, polls and runs only what it is told; one without runs nothing.
- With the store down no command runs and no traffic is set.
- `health` is never run twice within 10 seconds.
- Every report, poll change and result is in the log once.
- Under Production only the project rolls out like a staging one, with its CI read from commit statuses only when check-runs aren't readable.
- retaskable.com's own project rolls out from a computer with the two Cloudflare variables; a saved `site` runs nothing.
- `cargo fmt`, `cargo clippy --all-targets -- -D warnings` and `cargo test` pass.
