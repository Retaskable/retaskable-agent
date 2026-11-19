# One tap, or no stop at all: the agent's side — port note

Status: v0.1 (2026-11-19). The spec is Retaskable.com's [one-tap-or-none.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/one-tap-or-none.md) (the human's request, the **Obvious means** test, what is never skipped, the app's **Go with the recommended** and **Change it**); the TypeScript agent's half of it is retired, so what the agent does is specified here: [agent/06.2-implementer.md](agent/06.2-implementer.md) "One tap, or none" (the implementer and a piece's run) and [agent/06.5-spec-drafts.md](agent/06.5-spec-drafts.md) (the drafter).

What stays on the store's side and needs nothing from the agent: piece 1's check that every stop has a one-tap button (`quickAction.ts`), and piece 4 (Retaskable.com #4150, `shared/triage.ts`, `shared/backlogTriageOps.ts`): a stuck item whose only way on is **Carry on** carries on after the triager's second look. The store decides it from what the agent already reports (`triage` task results), so the agent has no change for it.

The store's side of the questions (Retaskable.com #4166): `run_done` `questions` and `draft_report` `questions` keep their options so the app shows buttons; `piece_note { piece, note }` writes a line on the item's timeline; the run brief carries `piece.tier` and `piece.checking`, and only `routine` with `auto` may be answered for.
