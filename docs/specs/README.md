# Agent specs

These are a snapshot of [Retaskable.com's specs](https://github.com/Retaskable/Retaskable.com/tree/main/docs/specs) as of its commit `4ad1e49371175ea576a8ce742c696cb5a77464d8`, trimmed for the agent. Retaskable.com's `docs/specs` is the source until the Rust agent replaces the TypeScript one: change behaviour there first, then copy the change here.

Start with [agent.md](agent.md), an index of sections in [agent/](agent/). Read the index, then only the sections you need.

## What is here

Only the specs whose subject is the computer itself:

- [agent.md](agent.md) and [agent/](agent/): the agent, its sessions, lanes, the orchestrator loop, its port and its config.
- [engines.md](engines.md): Claude Code, Cursor, pi and Codex as the engines sessions run on.
- [agent-settings.md](agent-settings.md): the computer's own settings page.
- [agent-telemetry.md](agent-telemetry.md): the watchdog and the metadata the agent records.
- [tray.md](tray.md): the tray, linking the computer, the terminal menu.
- [installers.md](installers.md): the installers, start at login and self-update.
- [docker.md](docker.md): the agent in Docker.
- [first-run-check.md](first-run-check.md) and [claude-signed-out.md](claude-signed-out.md): the checks the computer runs on itself.
- [local-state-diet.md](local-state-diet.md): the per-computer state file.
- [pieces.md](pieces.md): taking work as short runs on pieces, instead of claiming whole items.
- [daily-lighthouse.md](daily-lighthouse.md) and [compliance-scan.md](compliance-scan.md): the Lighthouse and scan runs (raw results only).
- [agent-ci.md](agent-ci.md): CI run on the agent computer, in Docker, on its own slot.
- [play-on-phone.md](play-on-phone.md): installing a project's Android app on a phone.
- [merge-queue-from-store.md](merge-queue-from-store.md): the store's queue decides whose turn it is to merge.
- [ci-watch-through-store.md](ci-watch-through-store.md): the CI watcher sends facts to the store and runs what it answers; gentler on GitHub; Run now through the store.
- [no-agent-flags.md](no-agent-flags.md): the agent reads no release flag, and a check keeps it so.
- [plans-say-why.md](plans-say-why.md): failed, dropped and already-under-way plans.
- [questions-and-triage.md](questions-and-triage.md): options for a question, triage tasks, the classifier, bugs typed as ideas.
- [testflight-restart-wait.md](testflight-restart-wait.md): a restart waits for a TestFlight build.
- [small-agent-jobs.md](small-agent-jobs.md): usage recheck, keep awake, native app facts, no half emoji.
- [restart-and-self-update.md](restart-and-self-update.md): restart and self-update.

- Where the server now decides, the agent copies keep only what the agent sends and runs, and link the server-side spec in Retaskable.com: [pools-on-server.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/pools-on-server.md), [merge-queue-on-server.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/merge-queue-on-server.md), [chores-on-server.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/chores-on-server.md), [jobs-on-server.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/jobs-on-server.md) and [rollout-on-server.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/rollout-on-server.md).

## What was cut

The project is thinning the agent: everything that belongs to the shared backlog store, `shared/` or the cloud moves out of it. The copies leave out what the store or hub should decide or compute, the no-store behaviour, local copies of shared data, and anything the server answers without a computer. [TRIMMED.md](TRIMMED.md) lists each cut and where it belongs (store op, hub or app).

## What is not here

Specs about the cloud, the store, the app or the site stay in Retaskable.com. Where the agent depends on them, the copies link to them there: the [backlog store](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/backlog.md), the [commands list](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/cloud.md) (cloud.md section 6), [code storage](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/code-storage.md), [computer tags](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/computer-tags.md), [CI when ready](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/ci-when-ready.md) and [the desktop app](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/desktop-app.md). The links are to `main`, so they can run ahead of this snapshot.

## Rules that hold in every spec here

- The server never sees or touches the user's code. The computer sends only derived results.
- Approved specs are committed to the user's repo, and spec content never goes to the store.
- The port binds `127.0.0.1` and never takes a command.
- Claude runs with `--permission-mode auto`.
- The agent reads no release flag: a new feature ships On ([no-agent-flags.md](no-agent-flags.md)).
- Every project has a shared backlog. There is no local mode, and the agent fails closed when it can't reach it.

Synced with Retaskable.com PR #1848 at commit 844314cf172b71774ab5cff10229f6f10fdcdbbf (engine per lane).

Synced with Retaskable.com PR #1850 at commit c825f06e15c8ce4025715208a57666ecc84f0220 (lanes are the computer's, shared by all its projects).

Synced with Retaskable.com PR #1860 at commit 8a682e8c8ea5f8badc1289933563e43eba926540 (Cursor's token use on the Usage card).

Synced with Retaskable.com PR #2008 at commit ee32ff7bf3c4cc5962904ce0af07aefb23741ad6 (triage: a second look that can carry out a clear suggestion, bug-report dedup via `sameAs`, not retrying a slice blocked on another item's unbuilt infrastructure, and retrying a failed triage look before waiting for a human).

Synced with Retaskable.com at commit a47f5a426 (2026-11-11), for the specs the server now owns (pools and pacing, the merge queue, chores, scheduled jobs and triage, rollout): the agent sections, agent-settings and agent-telemetry were read against Retaskable.com's changes of 2026-11-09 and 2026-11-10 and brought up to date. Other specs stay as of the earlier syncs above.
