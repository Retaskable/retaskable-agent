## 9. Which computer, and the slot

**Eligible**: a computer eligible for the project (#2425 section 4.1: the owner is in the organisation, the trust level covers the project, its login reaches the repo, it has not skipped the project), whose tags include the project's `needs` and the job's `needs`, with a free CI slot.

**[Proposal] CI has its own slot, `ciSlots`** (a computer setting, default 1, 0 turns CI off on that computer), **not a lane.** Why:
- A lane is a worktree, a session, a model and a spend (agent.md section 4.1). A CI run needs none of the three, so charging it a lane would idle a session's capacity for a process that costs CPU only, and would let a long session block CI that a review is waiting on.
- CI is on the critical path to every merge. If it shared lanes, a computer full of build sessions would hold the checks of the changes it just built.
- The cost is CPU and memory contention with the lanes on the same computer. That is the owner's to size (`ciSlots`, and nothing else in v1); the run starts with the lowest priority the OS lets a child take (nice 10 / below normal), so a session's checks win a contested CPU.

**Choosing the computer**: pull, as for pieces. A computer with a free slot asks the store for a CI job (`ci_take`), which hands out the oldest queued job it can take (needs matched), event priority first: the agent's own ready change and a staging commit before a base push before a person's pull request. **Warm affinity**: the store prefers a computer that ran this project's last CI (its caches are warm): other computers wait a short grace (20 seconds) before a job whose last computer is online with a free slot. This is a preference, never a wait: if that computer is busy or gone, the grace ends.

**The same computer as the change?** Allowed, and the common case with one computer. A change is not exempt from a clean-checkout run because the same computer built it; the mirror and worktree make it as clean as any.

**No computer on**: the status stays `pending`, "Queued: waiting for a computer to run the checks" (section 6). The agent's own merge for that change waits, as it waits on a host CI that has not started; its `awaiting_ci` 10-minute "no checks showed up" grace does not apply to `retaskable` (the store knows the run exists) and is replaced by the 2-hour pending limit, which on expiry asks a person ("CI has been queued 2 hours: no computer is taking it") rather than merging unchecked.

**Waiting on the project's `needs` does not fail**: the run waits (like items do, computer-tags.md section 2: "Waiting for a computer that has it").
