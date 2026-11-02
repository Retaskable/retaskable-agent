# The agent as CI — Spec

Status: v0.1, draft (2026-11-01). Section 2's **[Decided]** lines are what the owner asked for; every **[Proposal]** line fills a gap and is the owner's call; section 15 lists the questions that are really open, each with a recommendation. Builds on [pieces.md](pieces.md) (runs, leases, run kinds), [engines.md](engines.md), [agent-settings.md](agent-settings.md) and, in Retaskable.com, [ci-when-ready.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/ci-when-ready.md), [github-workflow.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/github-workflow.md) section 4.4, [code-storage.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/code-storage.md) section 3.1, [computer-tags.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/computer-tags.md), [cloud.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/cloud.md) sections 6 and 7 and [project-settings-on-server.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/project-settings-on-server.md). It is written to agree with the open spec PRs Retaskable.com #2407 (item branches and a merge queue, `merge-queue.md`) and #2425 (projects that just work, eligible computers and trust levels, `auto-projects.md`). Section 14 is the companion list for Retaskable.com, which this change does not edit; per [README.md](README.md) behaviour is decided there first, so that list is what carries this spec over.

Theme: Chores and CI

Kind: new

This spec is split: each section below is its own file in [agent-ci/](agent-ci/). Read this index, then only the sections you need.

## 1. What it is

- [agent-ci/01-what-it-is.md](agent-ci/01-what-it-is.md)

## 2. Decisions

- [agent-ci/02-decisions.md](agent-ci/02-decisions.md)

## 3. The project's setting

- [agent-ci/03-the-project-s-setting.md](agent-ci/03-the-project-s-setting.md)

## 4. What runs

- [agent-ci/04-what-runs.md](agent-ci/04-what-runs.md)

## 5. Where it runs: a clean checkout

- [agent-ci/05-where-it-runs-a-clean-checkout.md](agent-ci/05-where-it-runs-a-clean-checkout.md)

## 6. Reporting to the host

- [agent-ci/06-reporting-to-the-host.md](agent-ci/06-reporting-to-the-host.md)

## 7. The run page and logs

- [agent-ci/07-the-run-page-and-logs.md](agent-ci/07-the-run-page-and-logs.md)

## 8. Which changes get a run

- [agent-ci/08-which-changes-get-a-run.md](agent-ci/08-which-changes-get-a-run.md)

## 9. Which computer, and the slot

- [agent-ci/09-which-computer-and-the-slot.md](agent-ci/09-which-computer-and-the-slot.md)

## 10. The gate

- [agent-ci/10-the-gate.md](agent-ci/10-the-gate.md)

## 11. Untrusted code: threat model

- [agent-ci/11-untrusted-code-threat-model.md](agent-ci/11-untrusted-code-threat-model.md)

## 12. Interplay

- [agent-ci/12-interplay.md](agent-ci/12-interplay.md)

## 13. Restarts, going offline, and what is never reported as a pass

- [agent-ci/13-restarts-going-offline-and-what-is.md](agent-ci/13-restarts-going-offline-and-what-is.md)

## 14. Companion changes in Retaskable.com

- [agent-ci/14-companion-changes-in-retaskable-com.md](agent-ci/14-companion-changes-in-retaskable-com.md)

## 15. Open questions

- [agent-ci/15-open-questions.md](agent-ci/15-open-questions.md)

## 16. Not in this step

- [agent-ci/16-not-in-this-step.md](agent-ci/16-not-in-this-step.md)

## 17. PR plan

- [agent-ci/17-pr-plan.md](agent-ci/17-pr-plan.md)
