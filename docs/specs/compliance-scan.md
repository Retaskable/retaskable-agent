# A wider compliance and accessibility scan — Spec

Status: v0.5 (2026-10-25: the probes' and sweep's views are now the store's own computation from a run's raw per-check and per-rule results, not the agent's; item `summaries-move-to-store`, [last-local-state.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/last-local-state.md) section 3.4). Trimmed for the agent: the rules data, the views, the public page, the Chores screen and the command's definition are the store's, the hub's or the app's; the probes, the sweep and the checks stay here. Previously v0.4. Section 3.5 (item chore-run-now) was written out in detail on 2026-10-19, every new line a **[Proposal]**. v0.4 writes out item scan-a11y-keyboard's checks in detail (section 3.6, every line of it a **[Proposal]**). The human answered the open questions on 2026-10-18 (section 2's **[Decided]** lines); the **[Proposal]** lines fill the gaps and are the human's call. Extends the daily Lighthouse chore ([daily-lighthouse.md](daily-lighthouse.md)), the Chores screen ([chores-tab.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/chores-tab.md)) and the public page's Health tabs ([public-redesign.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/public-redesign.md) section 3.3).

Theme: Orchestration

Kind: new

This spec is split: each section below is its own file in [compliance-scan/](compliance-scan). Read this index, then only the sections you need.

## 1. What it is

- [compliance-scan/01-what-it-is.md](compliance-scan/01-what-it-is.md)

## 2. Decisions

- [compliance-scan/02-decisions.md](compliance-scan/02-decisions.md)

## 3. Behavior

- [compliance-scan/03-behavior.md](compliance-scan/03-behavior.md): 3.1 Statuses · 3.2 The probes (`agent/src/securityProbes.ts`) · 3.3 The accessibility sweep (`agent/src/a11yScan.ts`) · 3.4 What goes up · 3.5 Run now (item chore-run-now) · 3.6 The checks axe can't make, in detail (item scan-a11y-keyboard)

## 4. Acceptance

- [compliance-scan/04-acceptance.md](compliance-scan/04-acceptance.md)

## 5. What it will likely find here

- [compliance-scan/05-what-it-will-likely-find-here.md](compliance-scan/05-what-it-will-likely-find-here.md)

## 6. Not in this step

- [compliance-scan/06-not-in-this-step.md](compliance-scan/06-not-in-this-step.md)
