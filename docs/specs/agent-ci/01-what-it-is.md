## 1. What it is

Today a project needs a CI somewhere else: GitHub Actions, GitLab CI or a Woodpecker the owner runs. Retaskable's own repo uses a Woodpecker server whose `.woodpecker/ci.yaml` grew `seen`, `turn` and `merge` steps to cope with the agent (ci-when-ready.md sections 7 to 9). A new project with no CI gets nothing: the lane's checks gate the change, and the code host shows no checks.

The agent already has everything a CI needs: a computer with the project's toolchain, the project's approved check commands ([agent/src/allowedCommands.ts](https://github.com/Retaskable/Retaskable.com/blob/main/agent/src/allowedCommands.ts) in the TypeScript agent), `git`, the host's CLI, and a way to stream a run live to the site. So **the Retaskable agent is the CI for any project run on Retaskable, and the default.** It runs the project's approved checks on a clean checkout of the exact commit, reports the result to the code host as a commit status for that commit, and the merge waits for it. A project that has a CI of its own keeps it.

Nothing here adds a server, a queue or a database: run records are rows in the shared backlog store (as pieces' runs are), logs stay on the computer, and the code host is told through `gh` or `glab`.
