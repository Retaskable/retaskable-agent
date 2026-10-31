# Pieces and Runs — Spec

Status: trimmed for the agent from Retaskable.com's [pieces.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/pieces.md) (PR Retaskable/Retaskable.com#2065) and [look-runs.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/look-runs.md) (PRs Retaskable/Retaskable.com#2164, #2170, #2184 and #2185), which are the source: the store's ops, the board and a person's actions are there. This copy is what the computer does.

Theme: Orchestration

Kind: new

Items:
- **run-loop**: Take work as short runs on pieces instead of claiming whole items: ask, beat, end, and drop old runs on start. Depends on: orchestrator-loop. Specs: [pieces.md](pieces.md) sections 2 and 3.
- **piece-build-runs**: Run a piece's `build` and `fix` runs through the implementer, checks and review, with the piece as the implementer's brief. Depends on: run-loop, implementer-session. Specs: [pieces.md](pieces.md) section 3.2.
- **piece-watch-merge-runs**: Watch a piece's CI and merge it with no lane, and take the merge and chore locks on beats. Depends on: run-loop. Specs: [pieces.md](pieces.md) sections 3.2 and 3.4.
- **piece-plan-runs**: Plan an item into pieces with a read-only planner session. Depends on: run-loop. Specs: [pieces.md](pieces.md) section 3.2.
- **piece-look-runs**: Look into a piece or plan that keeps failing with a read-only session, and say what's next: try again with instructions, start over, split, wait to retry, or ask a person. Depends on: run-loop. Specs: [pieces.md](pieces.md) section 3.5.
- **run-limits-and-causes**: End a run that hit its engine's usage limit as `limited`, so another computer takes it at once; flag a failure whose cause isn't the work's as `outside`; send the computer's engines with `run_take`, and run a piece on the engine it names. Depends on: run-loop. Specs: [pieces.md](pieces.md) sections 3.1 and 3.6.

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
- **Ask**: when a lane (or, for `watch` and `merge`, no lane) and the usage limits allow, send `run_take { agent, kinds }` for the kinds it can run now. `agent` carries the computer's tags, as a claim's did ([agent/07.5-what-a-claim-and-a-give-back-send.md](agent/07.5-what-a-claim-and-a-give-back-send.md) section 7.5), and its engines (section 3.6); a missing tag answers `{ waitingFor }` and nothing, and the agent asks again on `work_available`. A person's action on this computer asks by name, `run_take { agent, piece }`.
- **Run** in memory: `{ token, kind, piece, lane?, processes }`. The lane comes from the computer's pool, preferring the piece's last lane here for its warm build, and goes back when the run ends, whatever the outcome.
- **Beat**: `run_beat { tokens }` every minute with every run's and lock's token. An answer of `stop` for a token kills that run's process tree and ends it as `interrupted`.
- **End**: `run_done { token, outcome, result }`. On `stale_run` the work is thrown away and nothing else happens. Any other refusal is sent again a minute later. A run whose engine hit its usage limit ends `limited`, and a failure whose cause isn't the work's says `outside` (section 3.6).
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
| `look` | the base checkout as it is | A read-only session decides what's next for a piece or plan that keeps failing (section 3.5). One plan or look at a time per computer. | no | `looked` (decision), `failed` (why) |

The implementer's brief for a piece is the piece: its title, `what`, `doneWhen`, earlier attempts' reasons, answers and any `next`. Rule 1 says to build that piece whole and nothing else; the store, not the result, says what's left of the item. A piece too big for one change ends `failed` with how to split it, until the implementer can return `split`.

### 3.3 Ceilings

A run past its kind's ceiling is lost in the store even while it beats: `build` and `fix` 3 hours, `watch` 6 hours, `plan`, `look` and `merge` 1 hour. The agent ends its own runs before that, as `failed` with why.

### 3.4 Locks

The merge lock and chore locks are `lock_take { name, note? }` and `lock_drop { token }`, renewed in the same `run_beat` and lost 3 minutes after the last beat (or past 1 hour for the merge lock, 6 otherwise). `runs_drop` lets them go too.

### 3.5 Looks

The store gives a piece a `look` run after 3 failed runs of the same kind in a row (or at once, for an `outside` failure), and an item whose plan failed 3 times the same way. The agent asks for `look` in `run_take`'s `kinds` once it can run one; until then such a piece waits for a person.

The session runs on the middle tier with the reviewer's budget and the `triager` role's engine, read-only, like the triager ([agent/06.9-triage-claude-looks-at-needs-you-first.md](agent/06.9-triage-claude-looks-at-needs-you-first.md)). Its prompt, `prompts/look.md`, is built from `prompts/triage.md`. Its brief is what `run_take` returns: the piece's title, `what` and `doneWhen`; every attempt's kind, outcome, why, computer and engine; its branch and change, if any; earlier looks' decisions and reasons; for a plan, the item's last 10 history notes; and the known Retaskable bugs. It may read the code, `git` history and the change on the code host. It never sees another computer's transcripts.

It returns `{ decision, reason, instructions?, pieces?, engine?, retaskableBug? }`, sent as `run_done`'s `looked` result. `decision` is one of:
- `again`: the runs need telling something; `instructions` go into the next runs' brief.
- `start_over`: the pushed branch is a dead end.
- `split`: `pieces` replace the piece; for a plan, it plans again with `instructions`.
- `wait`: the cause isn't the work's (Retaskable's own bug, a service down, the computer's setup); the piece waits to retry, with `reason` as what it waits for.
- `person`: it needs a person's decision or access; `reason` says what.

The store carries the decision out. `engine` (`claude`, `cursor`, `pi` or `codex`), with `again`, `start_over` or `wait`, moves the piece to that engine when the failures were the engine's own. `retaskableBug` is reported as the triager's is. A look must not pick `again` or `start_over` for work another, unbuilt item owns, nor repeat a decision an earlier look made that then failed the same way.

### 3.6 Limits, outside causes and engines

- **A usage limit isn't a failure.** A run whose session hit its engine's usage limit ends `limited { engine, until? }`, `until` being when the limit lifts, if the engine said. The store gives the piece to another computer at once, with no attempt counted, and gives this one no runs needing that engine until then. The agent doesn't hold the run waiting for the limit. A plan or look at the limit ends the same way.
- **Outside causes.** A `failed` run's result carries `outside: true` when plain code recognises a cause that isn't the work's: a prompt variable it didn't fill, the engine failing to start (`spawn_error`) or to sign in, a permission denied on the lane or its git folder (`EACCES`, `EPERM`), or the code host unreachable (`ECONNREFUSED`, `ENOTFOUND`, `ETIMEDOUT`, `Could not resolve host`). The store then gives the piece a look at once.
- **Engines.** `run_take`'s `agent` carries `engines`, the engines installed here (Claude only while signed in), and `uses`, the engine each kind's role runs on (`plan`: planner, `build` and `fix`: implementer, `look`: triager). A piece with its own `engine` runs its sessions on that engine, whatever the role would pick.

## 4. What it changes in other items

- **restart-cuts-sessions**: the process trees still die whole, and a stop or restart still cuts them; but no item is kept to resume. Its runs end `interrupted` through `runs_drop`, and the store gives them to whoever asks next.
- **claim-tags-and-give-back-note**: the tags go with `run_take`, as section 3.1 says. There's no give-back: a run ends with `run_done`, its why in its result.
- **item-waits-and-finishes**: `nothing_left` is the run's `nothing_left` outcome. A `waits_on` result ends the run `failed`, with the items it needs as its why (decided 2026-10-30), until runs have an outcome of their own for it.
- **resume-fresh-start-rules**: only within one run, where a fix round resumes the implementer's session. A new run always starts fresh.
- **detached-implementers** is dropped: a restart waits for its runs or ends them, so there's nothing to attach to afterwards.
- **implementer-session**: unchanged, but its brief for a piece is section 3.2's.
