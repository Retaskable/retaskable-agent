# Restart and self-update — Spec

Status: v0.1, draft (2026-11-08). Plans the restart and self-update pieces the Rust agent has not ported. Builds on [agent.md](agent.md) sections 4.2, 7.3, 7.4, 8 and 9.3 ([agent/04-architecture.md](agent/04-architecture.md), [agent/07.3-retaskables-own-failures.md](agent/07.3-retaskables-own-failures.md), [agent/07.4-stopping-and-restarting.md](agent/07.4-stopping-and-restarting.md), [agent/08-http-api.md](agent/08-http-api.md), [agent/09.3-what-the-computer-does-when-a-command-asks.md](agent/09.3-what-the-computer-does-when-a-command-asks.md)). Backlog: `restart-relaunch`, `restart-flush-wait`, `restart-hold`, `restart-on-code-change`, `update-pending-gate`, `self-hosted-update`, `update-to-main`, `spawn-join-cut`, `crash-kills-group`, `own-failure-flag-on`. New lines are marked **[Proposal]** unless the request fixed them.

Theme: Orchestration

Kind: new

## 1. What this is

`restart.rs` runs the restart sequence (cut every session, stop other sessions, exit with code 75), but the pieces around it are missing: nothing starts the agent again, the flush before exit is a timer, no code change restarts it, nothing takes the finish-first hold, and Retaskable cannot build itself. The behavior is the TypeScript agent's, already written in the sections linked above; this spec names the slices and the order. Where a section already describes the behavior, it is the source and this spec does not repeat it.

## 2. Decisions

- **[Decided]** All the gaps in the request are planned as separate items, in the order of section 3.
- **[Decided]** The flag decision (slice 10) must be made: with `own-failure-handling` Off every failure goes to a person.
- **[Proposal]** Turn `own-failure-handling` On for everyone, then delete the flag and its Off paths. The Off behavior is the bug 07.3 exists to fix ("Needs help" means a person is really needed), and nothing keeps a reason for it Off.
- **[Proposal]** Exit code 75 stays the one restart signal for every way of relaunching.

## 3. Behavior, in build order

1. **Relaunch after exit 75 (`restart-relaunch`).** A checkout run needs a supervisor: a small `retaskable-agent supervise` mode (or a wrapper in `main.rs`) that runs the agent as a child and starts it again on exit 75, with the current binary, and stops on any other exit code. A release build starts its next copy itself ([installers.md](installers.md) section 3.8: the `restart_handler`). Uses `std::process` and `std::path` only; Windows first, Linux and macOS too. The settings page's Restart then brings the agent back with no person. Tests: a fake child exiting 75 then 0 gives two launches; any other code gives one.
2. **Flush on the link's answers (`restart-flush-wait`).** `runtime::cut_every_session_then_exit` replaces the fixed 200 ms wait with waiting until the cloud link has sent the answers it holds (the restart command's own answer first), with a ceiling of 5 seconds, **[Proposal]**, so a link that cannot be reached cannot hold the restart. Tests: an answer queued before the restart is sent before the exit; a stuck link exits at the ceiling.
3. **The finish-first hold (`restart-hold`).** A restart waits for a merge, a promote or an update in progress to finish, then goes (agent/04-architecture.md section 4.2: "now, finishing a merge first"). Add the hold to `restart.rs`'s `RestartState`: merge, promote and update each take it when they begin and give it back when they end (also on error or panic, so it cannot stick), and the pending restart exits only with no hold taken. The view's restart progress says what it is finishing ("finishing a merge first", [agent/09.2-what-the-computer-reports.md](agent/09.2-what-the-computer-reports.md)). Tests: a restart asked mid-merge exits after the merge; a failed merge still frees it.
4. **Restart when its own code changes (`restart-on-code-change`).** Port `rememberCode` / `restartIfCodeChanged` from `selfUpdate.ts`: at start, remember the running build's identity (a checkout: the hash of the files that make up the agent, `src/`, `prompts/`, `Cargo.lock`; a release build: its version); after an update or a fetch that moved the checkout, compare and, if different, ask for an `update` restart. Needs slice 1 to land anywhere. Tests: a changed `prompts/` file asks for a restart; an unchanged tree does not.
5. **No new work while an update restart waits (`update-pending-gate`).** Port `canStartSession`: the driver neither claims nor starts an implementer while an `update` restart is pending; other restarts let implementers start, as 07.4 says. Reads `RestartState` from slice 4. Tests: pending update blocks `claim`; a plain restart does not.
6. **Retaskable building itself (`self-hosted-update`).** Port `selfHosted` (the project's `repoPath` is the checkout the agent runs from), `selfBehind` (each fetch counts commits behind `origin/<baseBranch>` and reports the count) and `catchUp` (fast-forward only after a merge or a behind fetch, once per commit of `origin/<baseBranch>`, refused when the checkout is off the base branch, dirty in tracked files, or ahead), with the update line "Updated to <base> (<sha>)", plus "; restarting now" when the code changed, written as agent/04-architecture.md section 4.2 says. A changed `Cargo.lock` needs nothing run on the machine. Takes the hold of slice 3 while it updates and asks for the restart of slice 4. Tests: each refusal reason; a fast-forward; a second attempt for the same commit is skipped.
7. **The update to main command and Ask Claude's restart (`update-to-main`).** The **update to main** command (`src/commands.rs`) runs the slice 6 update on demand and answers with its line or its refusal reason (for the button's tooltip). Ask Claude's `restart` action always restarts after the update, even when it pulled nothing ("Up to date with main; restarting now"), and only after the ask's answer is saved and its other actions have run (`restartSoon`), so the restarted agent does not answer the ask again.
8. **Close the spawn-to-join gap (`spawn-join-cut`).** Today a restart asked between `spawn` and the tree joining the registry misses the tree (07.4, "Two cuts at once"). **[Proposal]** Do as that note says: `spawn_and_read` reads `stopping()` right after joining and cuts its own tree if set. Then change the note from a Proposal to as built. Tests: a stop raised before the join leaves no child alive.
9. **A crash must not leave the process group running (`crash-kills-group`).** On Windows the Job object's kill-on-close already does it. On Linux and macOS the group outlives an agent that crashes or is killed. **[Proposal]** Linux: set `PR_SET_PDEATHSIG` (`SIGKILL`) in the child before exec, and have the group's leader do the same; macOS has no such call, so each child is started through a tiny watcher thread/pipe: the child group is killed when the pipe from the agent closes, and a startup sweep kills groups left in the `live` records of a previous run. Tests: killing the agent with `SIGKILL` leaves no child (Linux, macOS CI only; skipped on Windows).
10. **Decide the own-failure flag (`own-failure-flag-on`).** See section 2: **[Proposal]** make `own-failure-handling` always On, delete `Flags` checks for it in `settle.rs`, `item_commands.rs` and the tests that set it, and rewrite 07.3 and 07.4's Rust paragraphs from "behind the flag" to as built. A tweak: no new behavior, only the Off paths go.

## 4. Slices and acceptance

Each numbered piece in section 3 is one reviewable pull request, built in that order except that 8, 9 and 10 depend on nothing and can go any time.

- A Restart from the settings page brings the agent back by itself on a checkout and on a release build.
- A restart waits for the link's answers and for a merge, promote or update in progress, and never longer than its ceilings.
- A pushed change to the agent's own code restarts a self-hosted agent, with no new implementer starting in between.
- The update to main command and Ask Claude's restart give the lines and refusals in agent/04-architecture.md section 4.2.
- No restart leaves a child running on any platform, and a crash of the agent leaves none on Linux or macOS.
- No `own-failure-handling` flag is left in the source.
