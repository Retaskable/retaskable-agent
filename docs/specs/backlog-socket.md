# The store socket

Port of the TypeScript agent's `agent/src/backlogSocket.ts` (Retaskable.com's [backlog/06.3-push-leases-and-crashes.md](backlog/06.3-push-leases-and-crashes.md) and the "The socket" bullet of [backlog/15-as-built-2.md](backlog/15-as-built-2.md)). Code: `src/backlog_socket/` (`mod.rs` the loop and the push shapes, `transport.rs` the WebSocket behind a trait, `wake.rs` what a push wakes, `live.rs` the real sockets). Only the code agent runs it: the Chat Agent and the CI runner don't open a backlog socket in the TypeScript agent either, and the three programs coordinate only through the store ([programs.md](programs.md)).

## 1. What it is

- One socket per project on GitHub: `GET <store>/v1/repos/<repo id>/socket` (`wss` for an `https` store) with the store's access token in `Authorization: Bearer`. The token is the machine's one access token for the store, swapped from its refresh token as the Chat Agent's and the CI runner's are (`DeviceTokens`, `BACKLOG_TARGET`); a connect that never opened asks for a fresh one next time.
- **While it is open the computer's leases don't run out** (the store gives a lease no expiry while its holder's socket is open, and 15 minutes after the last closes: backlog/06.3). That is all the agent does for lease keep-alive: it sends `ping` every 50 seconds (under Cloudflare's 100 s idle cutoff; the store answers `pong` without waking the object). A report or beat still resets the store's idle hour.
- It reconnects with the cloud link's backoff (1 s, 2 s ... 60 s, up to half taken off at random; a connection that lasted 30 s starts it over). Not linked to the store, it looks again once a minute. The store's `4401` (this agent was unlinked on the store's `/agents` page) asks for a token at once with the refused one named stale, which forgets the link; until it's linked again it looks once a minute.
- Outbound only, and it only listens. Operations stay on HTTP (`backlog::Client`). The HTTP port is not involved.

## 2. What the store pushes

Each is one JSON text frame. A frame that isn't JSON, has an unknown `type` or lacks a field it needs is ignored; `pong` is ignored.

| `type` | Fields | Parsed as |
| --- | --- | --- |
| `work_available` | | `Push::WorkAvailable` |
| `changed` | `items: string[]` | `Changed` (only strings are kept) |
| `drafts_changed` | `drafts: string[]` | `DraftsChanged` |
| `settings_changed` | `version: number` | `SettingsChanged` |
| `paused_changed`, `tryouts_changed`, `ideas_changed` | | the same names |
| `tryout_problem` | `key: string`, `notes?: string` | `TryoutProblem` (`notes` is empty when missing) |
| `draft_answer` | `id: string` | `DraftAnswer` |
| `facts_changed` | `kinds?: string[]` | `FactsChanged` (`kinds` is empty when missing: any may have changed) |

Each opening is also a `Push::Reconnected`: anything pushed while the socket was down was missed.

## 3. What a push wakes

A push is never a command and carries nothing the agent acts on: it ends a wait, and the loop then reads the store, whose answer is all that is acted on (`WorkSignals`, `src/work_signal.rs`, one `Notify` per project and kind: a wake sent before its loop listens is kept, and several count as one).

- `work_available`: the project's run loop asks for work again (the same wake the cloud link sends; either may come first).
- `facts_changed` naming `expert_queue`, or naming nothing: the expert sessions' beat is due now, so the site's edits, cancels and messages (and another computer taking one) are read back with `pull()` without waiting out the minute ([expert-chat.md](expert-chat.md) "Beat").
- `facts_changed` naming `deploy_requests`, or naming nothing: the deploy request loop lists the requests now instead of at the next 5 s read ([agent/deploy-requests.md](agent/deploy-requests.md) section 2, List).
- `Reconnected`: all three.
- Everything else is heard and dropped. The TypeScript agent's `pushed` (`agent/src/storeLink.ts` 131-190) also reads the facts, the pause, the try-outs, the ideas, the settings and the drafts, and calls the intent puller on a push; here the intents are pulled every poll, and the pause, the settings, the facts and the draft sync are read on the loop's minute, so those pushes wake nothing yet.

## 4. Fallback and failing closed

The socket only makes things earlier. With it down (not linked, the store out of reach, a refused token) every timer and poll is as it was without it: the beat every minute, the deploy list every 5 s, `claim_next` and the run loop on their own wake and pass. A missed push is a later read, never a missed change. Nothing is claimed, run or sent because of a push, no push widens what the agent accepts, and a store that can't be reached still means nothing is claimed.

## 5. Differs from the TypeScript agent

- **A connect times out** after 30 s, as the cloud link's does. `backlogSocket.ts` 157 has no limit, so a connect that neither opens nor fails holds the socket off for good.
- **Project grants aren't ported**: a project that isn't on GitHub (its own backlog, `/v1/projects/<client>/<project>/socket`, code-storage.md 3.5, 3.6.1), the `{ type: "grant" }` renewal and the closes `4403` and `4408` (`backlogSocket.ts` 4-6, 141-143, 187, 213-223). This agent names a backlog only by its repo's id.
- **No `reconnect()`** (`backlogSocket.ts` 100): after linking, the once-a-minute look takes the link up.
- **Fewer wakes** (section 3).
- The access token comes from `DeviceTokens`. The code agent's other store calls (`src/orchestrator/driver/read.rs`, `src/deploy_requests/live.rs`) still send the stored refresh token itself as the bearer; the socket doesn't copy that.

## 6. Tests

`src/backlog_socket/tests.rs`: every push shape and every ignored frame; which push wakes which loop; on virtual time and channels (no network): the URL and token, `reconnected` then pushes, `ping` every keep-alive and the clean close on stop, the minute's look while not linked, backoff growth with the stale token, a connect that hangs, the backoff starting over after a good connection and not after a short one, `4401`, no repo id; and the real transport against a loopback server (the `Authorization` header, text both ways, the close code). `src/orchestrator/driver/tests/experts/wake.rs`: a push reads the queue before the beat is due, and only its own project's and kind's. `src/deploy_requests/tests.rs`: a push lists at once and the timer goes on without one.

## 7. How many calls this computer sends the store (2026-11-20)

The store limits operations per agent (120 a minute) and per person (600 a minute), counted per Cloudflare location (Retaskable.com `backlog/src/rateLimits.ts`, answered 429 "Too many requests to the backlog: wait a minute", `Retry-After: 60`). An agent is a computer, not a project, so every project this computer serves shares the one 120. With five projects each polling on its own, the agent sent more than that and the store refused all of them for a minute at a time (144 refusals in the 4 minutes after a restart, then about 13 a minute).

- **[Decided]** One budget for each store, for the whole process (`src/backlog/budget.rs`): a token bucket of 90 calls a minute with a burst of 12, first come first served. Every call of every project (`Client::post_op` and `get_bytes`) and the CI runner's (`StoreOps::post`) takes its turn first. A store on this computer (127.0.0.1, localhost) is not limited.
- **[Decided]** A `429` from any call holds all of them: no call until its `Retry-After` has passed (a minute when it names none, at the most 5), then the queue goes at the bucket's pace. The limit is shared, so one project's refusal is every project's. A call that would wait more than 90 s is refused at once with a 429 naming the wait, and the run loop's own hold (`AskHold`, `ci_pass`'s) takes over. The wait is read from the store's own 429, which carries a code (`rate_limited`) as well as from a proxy's; before this it was read only from one without a code, so the loops' holds always fell back to a minute.
- **[Decided]** Reads a pass makes more than once are made once (`src/backlog/read_cache.rs`): `list`, `drafts` and `intent_list` are kept 5 s for each project and shared, a read under way is waited for, an error is never kept, and any other op to the project drops them. `intent_list` is asked for every kind (the store answers all without `kinds`) and each of the eight pullers takes its own kinds from the rows, so a pass is one call, not eight.
- **[Decided]** The startup calls (`runs_drop`, `merge_withdraw`, the held drafts) are kept and paced by the same budget, so five projects' startups no longer land in the same second.
- Not changed: `health` (a minute a project) and the polls that ask the store for work, which the loops already hold on an empty answer. The store's limit is not too low for a computer with a handful of projects: after this a project costs about 6 to 8 calls a minute, so 90 covers ten or so.
