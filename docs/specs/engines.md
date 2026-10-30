# Engines: Claude Code, Cursor, pi and Codex — Spec

Status: v0.3, draft (2026-10-26, section 12 only). Builds on [agent.md](agent.md) sections 2, 4, 6 and 7 (sessions, the orchestrator and model tiers) and section 9 (the Agents tab and Setup). Also builds on [projects.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/projects.md) (per-project settings) and [estimates.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/estimates.md) / [spec-map.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/spec-map.md) section 4 (the recorded cost).

Theme: Orchestration

Kind: change

This spec is split: each section below is its own file in [engines/](engines/). Read this index, then only the sections you need.

## 1. What it is

- [engines/01-what-it-is.md](engines/01-what-it-is.md)

## 2. The engine layer

- [engines/02-the-engine-layer.md](engines/02-the-engine-layer.md)

## 3. Choosing the engine

- [engines/03-choosing-the-engine.md](engines/03-choosing-the-engine.md): 3.1 Per computer and per role · 3.2 Models and the usage limit · 3.3 Resuming · 3.4 Cursor model names · 3.5 When an engine hits its limit, the other takes over

## 4. Running `cursor-agent`

- [engines/04-running-cursor-agent.md](engines/04-running-cursor-agent.md)

## 5. Structured results from the final message

- [engines/05-structured-results-from-the-final.md](engines/05-structured-results-from-the-final.md)

## 6. Permissions: the allowlist

- [engines/06-permissions-the-allowlist.md](engines/06-permissions-the-allowlist.md): 6.1 Where it lives · 6.2 What it allows · 6.3 The rules · 6.4 Extra allow entries

## 7. Unknown cost

- [engines/07-unknown-cost.md](engines/07-unknown-cost.md)

## 8. Live transcripts

- [engines/08-live-transcripts.md](engines/08-live-transcripts.md)

## 9. Setup

- [engines/09-setup.md](engines/09-setup.md)

## 10. Build order

- [engines/10-build-order.md](engines/10-build-order.md)

## 11. Acceptance

- [engines/11-acceptance.md](engines/11-acceptance.md)

## 12. Open questions

- [engines/12-open-questions.md](engines/12-open-questions.md)

## 13. The pi engine

- [engines/13-the-pi-engine.md](engines/13-the-pi-engine.md)

## 14. The Codex engine

- [engines/14-the-codex-engine.md](engines/14-the-codex-engine.md)

## 15. Smart and Fast models

- [engines/15-smart-and-fast-models.md](engines/15-smart-and-fast-models.md): 15.1 Decisions · 15.2 The models a computer has · 15.3 The catalog · 15.4 Kinds of work · 15.5 Which model a session gets · 15.6 Lanes, and lanes from the plan · 15.7 Basic · 15.8 Expert · 15.9 Where it's saved · 15.10 Moving over · 15.11 Open questions · 15.12 What goes · 15.13 Build order and tests
