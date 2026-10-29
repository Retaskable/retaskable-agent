# When Claude's sign-in runs out — Spec

Status: v0.3 (this copy keeps the agent's part; the banner, bell, card and Sample are the app's). Item claude-signed-out-hold. Builds on the computer-wide hold for Cursor's limit ([engines.md](engines.md) section 3.2), Retaskable's own failures ([agent.md](agent.md) section 7.3), the `claudeSignedIn` check ([first-run-check.md](first-run-check.md) section 3.1) and the banners under the status strip ([pause-banner.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/pause-banner.md)). Section 2's **[Decided]** lines are what the human asked for; the **[Proposal]** lines fill the gaps and are the human's call. Showing the computer offline while it's held, and telling its owner, are [signed-out-offline.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/signed-out-offline.md)'s.

The TypeScript agent in Retaskable.com is the ground truth for behaviour (`agent/src/…` paths below). This Rust agent ports the same behaviour as its pieces land: section 3.1's classification lives in `src/engines/` (`signed_out_text` / `is_signed_out` / `describe_failure`); the hold (3.2–3.4) still waits on the pool and orchestrator, and there is no pool, orchestrator, steps or `signedOut` module, so the computer-wide hold (sections 3.2–3.4) is not on this computer.

Theme: Orchestration

Kind: new

## 1. What it is

When Claude Code's own sign-in on a computer runs out, every Claude session there ends at once with an error like `Failed to authenticate: OAuth session expired and could not be refreshed`. Today that is an ordinary session error (agent.md section 5, "any state ─▶ needs_human … error"), so each item that tries a session goes to **Needs help** with "Implementer ended with error: …", one after another, and **Carry on** fails the same way straight away while the computer is still signed out. Nothing about the items is wrong: the computer needs signing in again.

This makes a signed-out Claude **the computer's problem, not the item's**, the way Cursor's usage limit already is (engines.md section 3.2): the computer stops starting Claude sessions, says once, in plain words, that Claude needs signing in on it, and once it's signed in again every item it held carries on by itself, including ones already at Needs help for this reason.

## 2. Decisions

- **[Decided]** An item whose session failed only because Claude's sign-in ran out can be carried on: it isn't stuck for good. The human asked for this on 2026-10-22 after tasks stopped with "Implementer ended with error: Failed to authenticate: OAuth session expired and could not be refreshed".
- **[Proposal]** A signed-out Claude holds the whole computer, not one item: no Claude session starts on it until it's signed in again. Cursor roles carry on, as they do while Claude waits for the usage limit.
- **[Proposal]** The items it hit don't go to Needs help. They wait, and carry on by themselves (their session resumed with "You were interrupted; continue.") once the computer is signed in. An item with nothing pushed is given back to the store's queue at once, for a computer that's signed in.
- **[Proposal]** The agent finds out it's signed in again by itself: it runs the `claudeSignedIn` check once a minute while held. Nobody has to press anything after signing in. (signed-out-offline slice 2: the check runs once a minute whether held or not, so a computer signed out with no session to fail is held too; section 3.3.)
- **[Proposal]** Items already at Needs help only for this reason (from before this lands, or because triage left them) are carried on by the same release.
- **[Proposal]** The person is told once per computer, not once per item, by the app and hub from the snapshot's `claudeSignedOut` (section 3.4). Signing in is the person's job at the computer: the app's **Sign in** button sends `tool_sign_in { tool: "claude" }` ([cloud.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/cloud.md) section 6), which runs on the computer at once with the agent's own checks. The website never sends a token or a code.
- **[Proposal]** Matching the error is plain code on the session's error text, never a model's call.

## 3. Behavior

What the agent does when a Claude session ends because Claude Code's sign-in on the computer ran out. TypeScript paths are Retaskable.com's; Rust paths appear as this agent ports each piece.

### 3.1 Telling it apart

- `signedOut(r)` in `agent/src/engines/engine.ts`, beside `describeFailure`: true when a Claude Code result (`engine` `claude`, or missing) isn't `ok` and its `error` matches `/failed to authenticate|oauth (session|token).*(expired|revoked)|not logged in|please run \/login|invalid api key|authentication_error/i` (`signedOutText`, which also reads an item's needs-help reason). The result gains `signedOut?: true` beside `limitHit`, set by the Claude reader (`agent/src/engines/claude.ts`) on a `result` line and in `noResult`, whose error carries the stderr. A usage-limit hit, a local-model session, a Cursor result and a hand-built result never are.
- `describeFailure` gives "found Claude signed out on this computer (<the error>)" for it.
- In this Rust agent: `signed_out_text` / `is_signed_out` / `describe_failure` and `SessionResult.signed_out` (beside `limit_hit`) in `src/engines/engine.rs`. The Claude reader (`src/engines/claude.rs`) sets `signed_out` on a `result` line and in `no_result` the same way (`api_error_status` 429 or a rejected rate-limit on an error is a limit hit and never signed out; a `local` model session never is).

### 3.2 The computer holds

- **The hold** is the pool's, like `cursorLimitedUntil`: `Pool.claudeSignedOut: { since: number; error: string } | null` (`agent/src/pool.ts`), in memory, so a restart clears it and the first session tells again.
- A `signedOut` result from any Claude role sets it, in the session runner's `onEnd` (`holdClaude` in `agent/src/signedOut.ts`), logging `claude_signed_out` in every project's event log. `canStartSession` refuses while it's set, and so does the orchestrator runner's gate for every session on Claude Code, whatever gate its caller passed (drafts, asks and release notes pass `canPlan`); `canStartCursor` doesn't look at it, so Cursor roles carry on. `shouldStartNext` takes `canStartSession("implementer")`, so no new work is claimed for a Claude implementer while it's set.
- **The item** (`claudeSignedOut(o, id, followUp)` in `agent/src/steps.ts`, beside `cursorLimited`): the implementer's step, a scout or the final reviewer treats it as an interruption, not a failure, before the `spawn_error` and `exited` cases. An item with no PR and not a chore is given back to the store (`HostedWork.giveBack`, "Given back: Claude was signed out on <machine> before anything was pushed"). Otherwise it stays in its state with `followUp` kept (or `INTERRUPTED`; a review keeps its own) and `signInWait: true` (`ItemRecord`, local like `resumeAfter`, never sent to the store's details), its timeline noting "Claude is signed out on this computer; this carries on once it's signed in" once. `shouldLeaveLane` lets go of its lane, `advance` runs nothing for it, and its `doing` is `{ step: "paused", signIn: <machine> }` in any state (`doingOf`, `agent/src/pageShapes.ts`).
- **Triage** never runs on an item whose needs-help reason is a sign-out (`wantsTriage`), and a waiting item counts toward no rounds: nothing is incremented while it waits.
- **An ask** finds the hold and answers "Claude is signed out on <machine>, so I can't answer until it's signed in again." without starting a session (`agent/src/assistant.ts`). **A spec draft** doesn't start while held (`pumpDrafts`), and one whose session found it goes back to `queued`, resumed once released (`agent/src/drafts.ts`).
- This Rust agent has no `Pool`, session runner `onEnd`, or item step yet, so nothing holds Claude work for a sign-out on this computer.

### 3.3 Signed in again

- `checkSignIn(pool)` (`agent/src/signedOut.ts`), from every pool tick: once a minute (`SIGN_IN_CHECK_MS`, the first on the first tick), while the hold is set or any project runs a role on Claude Code (`usesClaude`, `agent/src/engines/choose.ts`), it runs the same check as `claudeSignedInCheck` (`claudeSignedInNow` in `agent/src/setupCheck.ts`, `claude auth status`). `true` clears the hold, logs `claude_signed_in` with how long it was out, and every item with `signInWait` drops it ("Claude is signed in again; carrying on") and carries on next tick, resuming its session with its `followUp`. `false` with no hold sets one (`since` now, "Claude Code isn't signed in", logged `claude_signed_out`), so a computer signed out while no session runs there is held, and shown offline ([signed-out-offline.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/signed-out-offline.md) section 3.1), within about a minute without a session having to fail first. `null` (couldn't tell) changes nothing. It tells the hub (`orchestrator_status`) only when the hold changed or an item was carried on. A computer whose projects run only on Cursor isn't checked.
- **After a restart**: the hold is in memory but `signInWait` is kept with the item, so the check also runs once after the agent starts when any project has an item still waiting: signed in, they carry on; signed out, the hold is set again; couldn't tell, they keep waiting.
- **Already at Needs help**: on that release, and once after the agent starts when any project has one, items at `needs_human` whose reason `signedOutText` matches (not running, no triage decision waiting: a person who acted has moved it on) are carried on with **Carry on** (`o.continue`), noted "Carried on: Claude is signed in again". At the start the check runs first: still signed out, it sets the hold, and Carry on makes them wait for the sign-in instead ("Carried on: it waits for Claude to be signed in on this computer").
- **Carry on** pressed while the hold is set (`continueItem`, `agent/src/orchestratorActions.ts`, implementer not on Cursor) doesn't start a session to fail again: an item going back to its implementer gets `signInWait` too, and its timeline says "Claude is signed out on <machine>. This carries on once it's signed in." (the `continue` command answers nothing, so the timeline says it). One going back to its checks runs them; its review then waits the same way.
- This Rust agent has no `setupCheck` / pool tick yet, so it does not re-check sign-in or clear a hold.

### 3.4 Telling the person

- **The snapshot**: the project's `orchestrator` gains `claudeSignedOut?: { machine: string; since: string }` (`shared/types.ts`, from the pool's hold in `agent/src/pageView.ts`); `State` reaches retaskable.com as it is, so `shared/cloud.ts` doesn't change. The orchestrator's status stays as it is (`running`, `paused` or `waiting_for_limit`); the hold isn't a new status. (Making room in `shared/types.ts`, the rollout types moved to `shared/moreTypes.ts`, re-exported.)
- **Told elsewhere**: the banner under the status strip, the bell line, the card's "Waiting for Claude to be signed in on <machine>", the Sample and the User Manual entry are the app's, drawn from `orchestrator.claudeSignedOut`. The hub shows a held computer offline and tells its owner ([signed-out-offline.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/signed-out-offline.md)). The agent supplies the snapshot field above and a `waiting for sign-in` marker on the item's `doing` (`{ step: "paused", signIn: <machine> }`), nothing more.
- This Rust agent has no page snapshot yet, so it does not publish `claudeSignedOut` or a sign-in `doing` marker.

### 3.5 Files and tests

- TypeScript (Retaskable.com): `agent/src/engines/engine.ts`, `agent/src/engines/claude.ts`, `agent/src/pool.ts`, `agent/src/signedOut.ts`, `agent/src/orchestrator.ts`, `agent/src/orchestratorActions.ts`, `agent/src/steps.ts`, `agent/src/review.ts`, `agent/src/triage.ts`, `agent/src/drafts.ts`, `agent/src/assistant.ts`, `agent/src/setupCheck.ts`, `agent/src/pageShapes.ts`, `agent/src/pageView.ts`, `agent/src/store.ts`, `shared/types.ts`.
- `agent/src/engines/claude.test.ts`: the error in the request, "not logged in", an `authentication_error` and a stderr's "Invalid API key" set `signedOut`; a usage limit, a budget hit, an ordinary error and a local session don't; a Cursor result never does; `describeFailure`'s words.
- `agent/src/signedOut.test.ts`: a signed-out result sets the hold once; no Claude session starts while held and a Cursor one does; the item waits with `signInWait`, lets go of its lane, shows paused for the sign-in and isn't triaged; an item with no PR is given back; a passing check (once a minute, not on "couldn't tell") clears it, a failing one sets it with no session having failed and a Cursor-only computer isn't checked, the waiting item carries on and one at `needs_human` with the error is carried on; at the start a still-signed-out computer is held; **Carry on** while held waits instead of starting a session.
- Rust (this agent): `src/engines/engine.rs` and `src/engines/claude.rs` (section 3.1's classification and its tests). The hold (sections 3.2–3.4) has no module yet.

## 4. Acceptance

- With Claude Code signed out on a computer (`claude auth logout`), the next Claude session there ends signed out, and that item doesn't go to Needs help: its card says it's waiting for Claude to be signed in on that computer.
- No other Claude session starts on that computer meanwhile, and no new item is claimed by it; a role on Cursor keeps working.
- After signing Claude in again on that computer, within about a minute the banner and the bell entry go and the held items carry on from where their sessions stopped, without anyone pressing anything.
- An item already at Needs help with "Implementer ended with error: Failed to authenticate: OAuth session expired and could not be refreshed" carries on by itself once the computer is signed in.
- TypeScript: `npm run test:changed` and `npm run lint` pass. Rust: `cargo test` for the modules that carry this behaviour.
