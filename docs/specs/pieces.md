# Pieces and Runs — Spec

Status: trimmed for the agent from Retaskable.com's [pieces.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/pieces.md) (PR Retaskable/Retaskable.com#2065), which is the source: the store's ops, the board and a person's actions are there. This copy is what the computer does.

Theme: Orchestration

Kind: new

Items:
- **run-loop**: Take work as short runs on pieces instead of claiming whole items: ask, beat, end, and drop old runs on start. Depends on: orchestrator-loop. Specs: [pieces.md](pieces.md) sections 2 and 3.
- **piece-build-runs**: Run a piece's `build` and `fix` runs through the implementer, checks and review, with the piece as the implementer's brief. Depends on: run-loop, implementer-session. Specs: [pieces.md](pieces.md) section 3.2.
- **piece-watch-merge-runs**: Watch a piece's CI and merge it with no lane, and take the merge and chore locks on beats. Depends on: run-loop. Specs: [pieces.md](pieces.md) sections 3.2 and 3.4.
- **piece-plan-runs**: Plan an item into pieces with a read-only planner session. Depends on: run-loop. Specs: [pieces.md](pieces.md) section 3.2.

## 1. What it is

The work is cut into small pieces up front, and a computer only ever rents one short run on one piece. The store holds everything but code; the code is on the code host. A run starts from something already pushed or stored and ends by writing its result to the store, so what it did locally is disposable: any computer can do any next run, and a run that fails, or whose computer dies, is run again elsewhere.

This replaces claiming an item for its whole life: the lease, parking, take-over, and resuming sessions by id after a restart. The Rust agent builds its loop on runs from the start and never claims items. A project it serves has `pieces: "on"` (source section 6).

## 2. Decisions

- **A run is the only hold.** It's held by a token, renewed by a beat every minute, and lost 3 minutes after the last one. Whether the cloud link is up doesn't matter.
- **Waiting holds nothing.** A piece waiting for CI, an answer, a try-out or a merge has no run.
- **A failed run is thrown away, not repaired.** The next run starts again from the pushed starting point, with the store's record of earlier attempts.
- **No session is ever resumed across runs.** A run that stops for any reason is started fresh next time. There are no session ids, transcripts or warm state to keep across a restart.
- **Lanes are the computer's own business.** A lane is the folder a run is in, held in memory for the run's life and never sent.

## 3. Behavior

### 3.1 The loop

The agent keeps no copy of a piece's state:
- **Ask**: when a lane (or, for `watch` and `merge`, no lane) and the usage limits allow, send `run_take { agent, kinds }` for the kinds it can run now. `agent` carries the computer's tags, as a claim's did ([agent/07.5-what-a-claim-and-a-give-back-send.md](agent/07.5-what-a-claim-and-a-give-back-send.md) section 7.5); a missing tag answers `{ waitingFor }` and nothing, and the agent asks again on `work_available`. A person's action on this computer asks by name, `run_take { agent, piece }`.
- **Run** in memory: `{ token, kind, piece, lane?, processes }`. The lane comes from the computer's pool, preferring the piece's last lane here for its warm build, and goes back when the run ends, whatever the outcome.
- **Beat**: `run_beat { tokens }` every minute with every run's and lock's token. An answer of `stop` for a token kills that run's process tree and ends it as `interrupted`.
- **End**: `run_done { token, outcome, result }`. On `stale_run` the work is thrown away and nothing else happens. Any other refusal is sent again a minute later.
- **Start**: `runs_drop` before the first `run_take`, which ends every run and lock this agent had as `interrupted`.
- **Stop and restart**: no new runs; wait up to 15 minutes for the runs to end, then kill the rest's process trees ([agent/07.4-stopping-and-restarting.md](agent/07.4-stopping-and-restarting.md) section 7.4, "Process trees die whole") and exit. `runs_drop` on the next start ends them. Nothing is kept to resume.

`state.json` keeps only the computer's own settings and caches: no runs, no tokens, no items.

### 3.2 The kinds

| Kind | Starts from | Does | Lane | Ends |
|---|---|---|---|---|
| `plan` | the base checkout as it is | A read-only `planner` session on the chore model returns the item's pieces, each `title`, `what`, `doneWhen`, `needs`. One at a time per computer. | no | `planned` (pieces), `failed` (why) |
| `build` | `origin/<base>` | The implementer, its checks, the scouts and review with their fix rounds; pushes and opens the change. | yes | `pr_opened` (branch, change), `questions`, `nothing_left`, `failed` (why) |
| `fix` | the piece's pushed branch and change | The same, told what to fix (failed CI, a conflict, a try-out's problem). | yes | as `build` |
| `watch` | the change | Reads its checks until they settle. | no | `green`; `red` (the failing checks, their links, what their logs say); `failed` when CI stays pending past the most it waits |
| `merge` | the change | Takes the merge lock and the merge turn, merges, or merges the base in and pushes. | no | `merged` (commit), `updated`, `conflict` (what the implementer would be told) |

The implementer's brief for a piece is the piece: its title, `what`, `doneWhen`, earlier attempts' reasons, answers and any `next`. Rule 1 says to build that piece whole and nothing else; the store, not the result, says what's left of the item. A piece too big for one change ends `failed` with how to split it, until the implementer can return `split`.

### 3.3 Ceilings

A run past its kind's ceiling is lost in the store even while it beats: `build` and `fix` 3 hours, `watch` 6 hours, `plan` 1 hour. The agent ends its own runs before that, as `failed` with why.

### 3.4 Locks

The merge lock and chore locks are `lock_take { name, note? }` and `lock_drop { token }`, renewed in the same `run_beat` and lost 3 minutes after the last beat (or past 1 hour for the merge lock, 6 otherwise). `runs_drop` lets them go too.

## 4. What it changes in other items

- **restart-cuts-sessions**: the process trees still die whole, and a stop or restart still cuts them; but no item is kept to resume. Its runs end `interrupted` through `runs_drop`, and the store gives them to whoever asks next.
- **claim-tags-and-give-back-note**: the tags go with `run_take`, as section 3.1 says. There's no give-back: a run ends with `run_done`, its why in its result.
- **item-waits-and-finishes**: `nothing_left` is the run's `nothing_left` outcome. A `waits_on` result ends the run `failed`, with the items it needs as its why (decided 2026-10-30), until runs have an outcome of their own for it.
- **resume-fresh-start-rules**: only within one run, where a fix round resumes the implementer's session. A new run always starts fresh.
- **detached-implementers** is dropped: a restart waits for its runs or ends them, so there's nothing to attach to afterwards.
- **implementer-session**: unchanged, but its brief for a piece is section 3.2's.
