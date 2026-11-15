# The expert queue on the server — Spec

Status: v0.1, draft (2026-11-14). Backlog item `expert-queue-pulled`. Copies the agent's part of Retaskable.com's [expert-queue-on-server.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/expert-queue-on-server.md) (sections 2, 3.1 and 3.2); the site and app part (section 3.3: Edit and Cancel written to the store) is the server's and is not copied. Builds on [expert-chat.md](expert-chat.md) (the queue of messages sent while a turn runs, and the continued Claude Code session) and, for the pattern copied, the draft lease in [agent/06.5-spec-drafts.md](agent/06.5-spec-drafts.md).

Theme: Orchestration

Kind: change

## 1. What it is

An expert session's pending messages (the ones sent while a turn runs) and its continuation note (the Claude Code session it continues) are **in the store**, not in `queue.json` and `continues.json` on the computer. The computer running the session pulls the messages: it takes a row, runs it, and reports it read. Editing or cancelling a queued message is a store write by the site and the app, so it works with the computer offline; the agent never sends `expert_queue_edit` or `expert_queue_cancel`. The engine's own session files (Claude's, Codex's, Cursor's) stay on the computer; only Retaskable's bookkeeping is in the store. This agent has no queue file to move over: it never wrote one.

## 2. Decisions

- **[Decided]** Pending expert messages are a store queue the agent pulls. The computer running the session takes them; the first taker wins, as `draft_take` does.
- **[Decided]** The continuation note is a store fact, `{ sessionId, title, branch? }`. It holds no path: the computer finds the Claude Code file from `sessionId` in Claude's own files. A computer without that session starts the first turn with the person's message alone.
- **[Decided]** A row is edited or cancelled only while nobody has read it; the store refuses it once `taken` ("That message was already read.").
- **[Proposal]** The agent keeps what it owes the store in memory and sends it when the store is back: `expert_queue_done` for rows it consumed, `expert_queue_clear` for a discarded session, `expert_continues_set` for a note. Nothing is written to disk and nothing is read from a stale file. With the store out of reach a message the agent could not write keeps a `local-` id and lives in memory for the run (fail closed applies to what the store decides; a turn already running is not cut off by an outage).

## 3. Behavior

### 3.1 The ops the agent sends

The store keeps the rows as the `expert_queue` fact `{ sessions: { <itemId>: [row] } }` and the notes as the `expert_continues` fact, written only by these ops (a generic `fact_set` of either is refused). A row is `{ id, text, pictureIds?, at, by, state, claim? }`: `state` is `queued` or `taken`, `claim { agentId, machine, at }` is set by the computer that took it, `text` is up to 4000 characters, at most 20 rows wait per session, and pictures are the store's picture ids, never data.

- `expert_queue_add { itemId, text, pictureIds?, by }`: a message sent while a turn runs, written as a row. `by` is the sender's login and must be the person who started the session (the item's `expert` login). Answers `{ id }`.
- `expert_queue_take { itemId, ids?, machine }`: the computer takes rows of a session it runs (it holds the lane or the parked item; a computer that does not run a session never takes its rows). A compare-and-set recording the agent and `machine`; the first wins, a second answers `conflict`. The same computer taking again only moves its claim's time on: that is the beat. Answers `{ rows }`, the rows now held, with the text the store has (the person may have edited a row meanwhile).
- `expert_queue_done { itemId, ids }`: the holder says the run has read these rows, and they leave the queue. Answers `{ ok: false }` when it no longer holds all of them.
- `expert_queue_clear { itemId }`: the session ended or was discarded; its rows and its note go.
- `expert_continues_set { itemId, from: { sessionId, title, branch? } }`: set once when the session starts. `sessionId` is `[\w-]{1,100}`.
- `expert_queue_list { itemId? }`: the rows (a stale claim shows `queued` again) and the notes, of one session or every session. The store pushes `facts_changed` with the kind `expert_queue` after a change.

A claim goes stale as a draft's lease does: a `taken` row whose computer has not beaten for `DRAFT_MOVE_MS` shows `queued` again, so no message is lost when a computer dies mid-turn.

### 3.2 What the agent does

- At start, when the link opens again, and on `facts_changed` naming `expert_queue`, the agent lists the rows of each session this computer drives and takes the oldest, then starts its turn. A message written to a Claude run's open stdin is `taken` and stays so until the reader reports it read, then `expert_queue_done` removes it.
- While it holds rows the agent beats (takes them again) once a minute. A `conflict` on a beat means another computer holds the row now, and this one lets go of it.
- A usage-limit wait, Stop, park and restart keep the rows in the store, so nothing is lost. Discard sends `expert_queue_clear`.
- Store unreachable: the agent keeps the rows it has read in memory for the run and sends its `take`, `done` and `clear` calls when the store is back (a `take` first, so a row taken while the store was out of reach is claimed before it is done; a `conflict` there means another computer has it and is let go).

## 4. Built so far

The store client and the queue that wraps it: `src/backlog/expert.rs` (the ops above) and `src/expert_queue.rs` (taking, beating, owing `done`/`clear`/notes to the store, local rows). The expert session itself (turns, lanes, the `expert_*` commands; see [expert-chat.md](expert-chat.md), "Built so far") calls into it: it adds the item (`expert_add`), takes and finishes rows in turn order, beats the lease with `lease_beat`, and reads the queue back with `pull()` once a minute (the `facts_changed` kind `expert_queue` is not wired yet).

## 5. Tests and acceptance

- The client sends each op with the shape the store checks, reads a `conflict` as a refusal, and an unreachable store as an error; a continuation note holds no path.
- The queue: three queued messages are taken and read in order; two computers produce one run; a take returns the text the store has now; a cancelled message is not taken; with the store down a message is local and the run carries on from memory; a `done` the store missed is sent when it is back and the row is hidden meanwhile; a discard and a note not sent go out on the next pull; the beat lets go of a row another computer holds.
- Acceptance: with the computer offline, the site's Edit and Cancel work and the message is read when the computer returns; no `queue.json` or `continues.json` is written; no path of a computer is in the store.
