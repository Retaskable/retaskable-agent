## 3. The project's setting

A new team setting, `ci` (a team field in `shared/projectSettings.ts` terms, not a command, so a member who may change settings may change it; the command fields it uses stay owner-gated):

| Value | Meaning |
|---|---|
| `retaskable` | The agent is the CI. **The default for a new project.** Merges wait on the `retaskable/ci` status. |
| `own` | The project's own CI (GitHub Actions, GitLab CI, Woodpecker, anything that reports checks to the host). The agent waits on the host's checks exactly as it does today (ci-when-ready.md, `awaiting_ci`). The agent runs no CI. |
| `both` | Both run; the merge waits for `retaskable/ci` and for every other check the host shows. For a project moving from one to the other, or one that keeps its own CI for deploys and wants Retaskable's for checks (section 12). |
| `none` | No CI wait: today's `requireCi` off. The lane's checks gate, as for a repo with no CI. |

**What existing projects get** **[Proposal]**: nothing changes by surprise. When the feature reaches a project, one computer works out a default once, from facts it can read (`ciCoverage` over the base branch's workflow files, and the host's required status checks on the base through the `rules` capability), and sends it as a settings offer (the way a scan's `needs` are offered):

1. The host shows required checks on the base, or `ciCoverage` finds CI configuration that runs for the base: **`own`**.
2. `requireCi` is off and neither of those holds: **`retaskable`** is *suggested*, not applied. The project's board shows "Run this project's checks on your computers as CI?" with **Turn on** and **Not now** (Not now sets `none`, which is what it did before). It is a suggestion because turning it on starts posting a status to every pull request and makes the agent's merges wait for it.
3. A project with `requireCi` on and workflows keeps `own`.

New projects (added after this ships) get `retaskable` with the setup in section 10, unless Setup finds CI configuration on the base, then `own` is suggested instead.

**Both at once** is allowed. The only cost is that every change waits for the slower of the two. It is a setting value, not a mode the agent guesses.
