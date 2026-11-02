# Expert Chat — the Rust port

Status: v0.1, draft (2026-10-30). Ports Retaskable.com's [expert-chat.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/expert-chat.md) sections 2, 3 and 8 (build order step 12, "The Rust port") to this agent, and carries over [personal-chat.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/personal-chat.md)'s note that the conversation is, as of this port, still the item's own history — personal-chat's own move of it to a person's chat store (its section 3.6) is a later step on the TypeScript side this port doesn't follow yet. Builds on [agent/05-item-lifecycle.md](agent/05-item-lifecycle.md), [agent/06-sessions.md](agent/06-sessions.md), [agent/07.4-stopping-and-restarting.md](agent/07.4-stopping-and-restarting.md), [agent/07.5-what-a-claim-and-a-give-back-send.md](agent/07.5-what-a-claim-and-a-give-back-send.md), [pieces.md](pieces.md) and [engines.md](engines.md).

Theme: Chat

Kind: new

An expert session is a person-driven coding session: it holds a lane and a branch the way an implementer's slice does, but across as many messages as the person sends, stops and restarts mid-step on the person's word, answers in-line questions, and can start its own helpers. It finishes the way any item's work does — ships through checks and review, goes back to the backlog, is parked, or is thrown away. Retaskable.com's own agent built this first and dogfooded it for a week before this port, so the decisions below are already **[Decided]** there; this spec only decides how they land in this agent's own types.

## Decisions

- **Expert sessions don't go through pieces.** [pieces.md](pieces.md) replaces claiming a whole item for its life — the lease, parking, take-over and resuming a session by id — with short, disposable runs on pieces any computer can pick up. An expert session is the opposite of disposable: one person holds it, by design, across many messages, and nothing about it is safe to hand to a different computer mid-conversation. **[Proposal]** it keeps using the item-lifecycle and lane-pool primitives directly (`src/item_lifecycle.rs`, `src/lane_pool.rs`, `src/lanes.rs`), the way the TypeScript agent's own `session.ts` does, rather than `run_take` / `run_beat` / `run_done`. A project running `pieces: "on"` for its roadmap work still takes expert sessions the old way, since an expert session was never a piece to begin with.
- **The state machine gains no new box.** `ItemState` (`item_lifecycle.rs`) models the automated build/check/review/merge path; an expert session only ever sits in `Queued` (parked) or `Implementing` (live), the two boxes expert-chat.md section 3.1 describes. **[Proposal]** the finer detail TypeScript tags onto the item (`expert_working` / `expert_asking` / `expert_idle` / `expert_parked`) is carried on the item's own run record as its own field, not as new `ItemState` variants — those four values describe a person's conversation, not a step in the pipeline, and the pipeline's own transitions (`implementer_finished`, `ci_finished`, ...) never fire for one.
- Everything else TypeScript decided 2026-10-30 carries over unchanged: starts free / takes over (parks the lowest-priority item in the lane first, waits up to `TAKE_WAIT_MS` 120s) / adds a lane; counts toward the daily budget but a running turn is never stopped by it, only warned at 80% and 100%; `expert_say` and `expert_stop` are the only two offered on MCP; only the computer's own linked person can drive one, and only on their own computer; idle for `expertIdleMinutes` (default 30) lets go of the lane on its own; helpers run the engine's own Fast tier automatically; the name stays "Expert", the item's `kind` is `expert`.

## Behavior, in build order

### 1. The item and its lane (`expert-item-lane`)

Beside `kind`, the item carries `expert: { startedBy, detail }` (`startedBy` a person id; `detail` one of `working` / `asking` / `idle` / `parked`) — `backlog.rs`'s `Item` has no such field yet and needs one, passed through the way `kind` already is (serde already leaves other unknown fields alone, so this is additive). Placing an expert's lane ports `session.ts`'s `placeExpert` / `takeLane` onto `lane_pool.rs`: free lane first (last one freed, first taken, `LanePool::free_lane`), else take over one (`LanePool::holder` finds the lowest-priority item holding a lane, that item is parked, then up to `TAKE_WAIT_MS` is spent waiting for it to actually let go; a lane that's `Merging` or already another expert's is refused), else add one (`LaneScaling`'s `scaled_out`, one added lane per computer, and the lane that would otherwise be freed next sits out while the added one is held). An idle session — no turn running, not asking — past `expertIdleMinutes` lets go of its lane on its own, the same way parking does.

### 2. Turns (`expert-turns`)

One `claude -p` (or `cursor-agent -p`, or a Codex thread) run per message, resumed on whichever engine the session started on. Ports `turns.ts`'s `turn` / `settle` onto this agent's engines layer: a fresh session for the first message, `--resume <id>` (or the engine's own equivalent) after, `role: "implementer"`'s tier and budget but `maxTurns` raised the way TypeScript's own default (300) is, and a structured result shaped like `EXPERT_SCHEMA` — `summary` required, an optional `question` with up to 4 options, an optional list of up to 4 helper tasks — read back the way `engines::json_result` already reads an implementer's. A turn ending with a question sets `expert.detail = asking`; one ending without sets `idle` and keeps `summary` as the session's last line; a run that failed or was stopped sets its own detail rather than `ItemState::NeedsHuman` — a person is already there watching, so nothing needs a human pulled in on top.

### 3. The commands (`expert-commands`)

`src/commands.rs`'s dispatcher gets five new match arms, each checked the way `refusal()` already checks every command — replay, expiry, setup-done — plus a check of its own: `expert_start` refuses off the driving person's own computer, and the other four refuse anyone but the session's own `startedBy` (`driverRefusal`). These are never routed through the MCP server's generic command path, which keeps exposing only `expert_say` / `expert_stop`.

- `expert_start { where, detail, laneItemId?, continue? }`: adds an item via the backlog store with `via: "expert"`, claiming it in the same step `claim_next` already claims one — **a new store op**, since `backlog.rs`'s `Client` has no "add an item" call yet — retries the id with a numeric suffix on a collision up to 20 tries, places its lane (piece 1), and starts the first turn (piece 2). `continue` starts that first turn from a Claude Code session already on disk instead of fresh; this piece's acceptance covers only the fresh-start path, since reading another session's transcript for a handover is its own, later unit of work.
- `expert_say { text }`: stops a running turn first, places the lane if the session is parked, then starts a turn.
- `expert_stop {}`: stops the running turn only; the session stays live and idle.
- `expert_helper { task, lane? }`: starts a helper (piece 5).
- `expert_end { outcome }`: `park` and `discard` are buildable now against `lane_pool.rs`'s `release` and `lanes.rs`'s `kill_processes_in` — park stops any helpers, lets go of the lane, and sets `expert.detail = parked` with `ItemState::Queued`; discard does the same and skips the item instead of requeuing it. **`ship` and `hand to backlog` are not buildable yet**: both need this agent to push a branch and open a PR through the code host, and `backlog.rs` has no such call anywhere (`agent/07-orchestrator-loop.md`'s own words: "checks, CI, push and merge aren't built"). This piece's spec describes them so the piece that eventually adds a publish capability has something to build against; its own acceptance only covers park and discard.

### 4. Steering, Claude only (`expert-steering`)

TypeScript's own build order builds this last (step 11) because it needs turns working first: a long-lived `claude -p --input-format stream-json --output-format stream-json --replay-user-messages` with stdin left open, so a message sent mid-turn is a stream-json line in rather than a stop-and-resume. **[Proposal]** this agent's engines layer has no long-lived, stdin-fed session shape yet (`runner.rs`'s `spawn_and_read` reads a finished process's output; it doesn't feed one lines while it runs), so this piece builds that shape for Claude specifically, falling back to stop-and-resume (prefixed "stopped you mid-step", matching `turns.ts`'s `MID_STEP`) for Cursor, pi and Codex, exactly as TypeScript's own fallback does.

### 5. Helpers (`expert-helpers`)

Ports `helpers.ts`: a helper is a `role: "scout"` session on the engine's Fast tier, either read-only in the expert session's own lane (`read` mode) or on its own lane branched from the session's last commit (`lane` mode, via `lane_pool.rs`'s `free_lane` — never a take-over or an added lane; it waits). Capped at `expertHelpers` (default 2) running at once; a helper can't start one of its own. Its result (`{ result: string }`) is delivered as a message to the session — at once if it's idle, held until the next person message if it's mid-turn — and stops with the session, on `expert_stop`, `expert_end`, or a restart.

### 6. The right panel's data (`expert-work-panel`)

Read from git after every turn, kept nowhere: files changed since the branch's merge-base (uncommitted ones included), the first 100 plus a count of the rest; the last 30 commits, each with whether it's pushed (left off a commit with nowhere to push). `src/lanes.rs` already knows the lane's path and its base; this piece adds the `git diff --name-status` / `git log` calls and shapes the result the way the website's panel expects it.

### 7. The bell question (`expert-bell-question`)

A turn's `question` becomes an `expert.ask` row the same way any other item's row reaches the shared inbox — sent to the session's own driver only, since nobody else on the project can answer it.

## On restarts

An expert turn runs under `role: "implementer"` (piece 2), so it falls under [agent/07.4-stopping-and-restarting.md](agent/07.4-stopping-and-restarting.md)'s "detached implementers" rule rather than "every other session is cut": its process is readopted after a restart — attached if still alive, resumed by session id if it finished while the agent was down, or its left-behind result answered — the same as a roadmap implementer's. **[Proposal]** a helper (piece 5), being `role: "scout"`, is cut like any other non-implementer session and is not resumed; its result, if any, is lost, the same as a scout's would be.

## Acceptance

Same bar TypeScript set for itself: a week of Retaskable's own work done in expert sessions, run through this agent instead of the TypeScript one, finishing by park or discard, with no loss of the person's place in a conversation across a restart.
