# Retaskable — Spec

Status: v0.5, trimmed for the agent: this is a copy of Retaskable.com's agent spec with what belongs to the store, the hub and the app cut out. See [TRIMMED.md](TRIMMED.md) for each cut and [README.md](README.md) for the other specs in this folder.

Theme: Orchestration

Kind: new

Items:
- **session-stop-interrupts**: Kill every running session's whole process tree when the agent stops, and keep the item to resume. Depends on: orchestrator-loop. Specs: [agent/07-orchestrator-loop.md](agent/07-orchestrator-loop.md) section 7.4.
- **resume-fresh-start-rules**: Start a fresh session when a resume can't carry on: transcript too large, out of turns, engine changed. Depends on: orchestrator-loop, implementer-session. Specs: [agent/06.2-implementer.md](agent/06.2-implementer.md) section 6.2.
- **item-waits-and-finishes**: Handle a session result of waits_on (record the dependency, give the item back) and nothing_left (mark it done). Depends on: orchestrator-loop, implementer-session. Specs: [agent/06.2-implementer.md](agent/06.2-implementer.md) section 6.2.
- **claim-tags-and-give-back-note**: Send this computer's tags with a claim, and a report note with every give-back. Depends on: orchestrator-loop. Specs: [agent/07-orchestrator-loop.md](agent/07-orchestrator-loop.md) section 7.5.
- **live-sessions-reporting**: Show the running sessions on the settings page's Now tab and count them in the cloud tick. Depends on: orchestrator-loop, settings-page, cloud-link. Specs: [agent-settings.md](agent-settings.md) section 4.5.

This spec is split: each section below is its own file in [agent/](agent/). Read this index, then only the sections you need.

## 1. What it is

- [agent/01-what-it-is.md](agent/01-what-it-is.md)

## 2. Principles

- [agent/02-principles.md](agent/02-principles.md)

## 3. Scope (v1)

- [agent/03-scope-v1.md](agent/03-scope-v1.md): 3.1 Working as a team

## 4. Architecture

- [agent/04-architecture.md](agent/04-architecture.md): 4.1 Lanes: parallel work without the disk cost · 4.2 Sandbox: agents in, game runs out

## 5. Item lifecycle

- [agent/05-item-lifecycle.md](agent/05-item-lifecycle.md)

## 6. Sessions

- [agent/06-sessions.md](agent/06-sessions.md)
- [agent/06.1-planner-retired.md](agent/06.1-planner-retired.md): 6.1 Planner (retired)
- [agent/06.2-implementer.md](agent/06.2-implementer.md): 6.2 Implementer
- [agent/06.3-review.md](agent/06.3-review.md): 6.3 Review
- [agent/06.4-playtests.md](agent/06.4-playtests.md): 6.4 Playtests
- [agent/06.5-spec-drafts.md](agent/06.5-spec-drafts.md): 6.5 Spec drafts
- [agent/06.6-ask-claude.md](agent/06.6-ask-claude.md): 6.6 Ask Claude
- [agent/06.7-chores-never-idle.md](agent/06.7-chores-never-idle.md): 6.7 Chores: never idle
- [agent/06.8-galleries-the-game-photographs-itself.md](agent/06.8-galleries-the-game-photographs-itself.md): 6.8 Galleries: the game photographs itself
- [agent/06.9-triage-claude-looks-at-needs-you-first.md](agent/06.9-triage-claude-looks-at-needs-you-first.md): 6.9 Triage: Claude looks at "needs you" first
- [agent/06.10-spec-layout.md](agent/06.10-spec-layout.md): 6.10 Spec layout

## 7. Orchestrator loop

- [agent/07-orchestrator-loop.md](agent/07-orchestrator-loop.md): 7.1 Model tiers · 7.2 Local model · 7.3 Retaskable's own failures

## 8. HTTP API

- [agent/08-http-api.md](agent/08-http-api.md)

## 9. What the app and site ask of the computer

- [agent/09-web-ui.md](agent/09-web-ui.md): 9.1 What the computer works out for a project's settings · 9.2 What the computer reports · 9.3 What the computer does when a command asks

## 10. Config — `.retaskable/config.json`

- [agent/10-config-retaskable-config-json.md](agent/10-config-retaskable-config-json.md)

## 11. Build milestones

- [agent/11-build-milestones.md](agent/11-build-milestones.md)

## 12. Open questions

- [agent/12-open-questions.md](agent/12-open-questions.md)
