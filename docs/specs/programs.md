# The three programs and where they split

This repository builds three programs. They run together on one computer today and must stay easy to run apart: the CI runner may already be on another server, and the Chat Agent may follow.

| Program | Binary | Code |
| --- | --- | --- |
| Code agent | `retaskable` | everything not listed below, including `src/main.rs` |
| Chat Agent | `retaskable-chat-agent` | `src/chat`, `src/bin/chat_agent.rs` |
| CI runner | `retaskable-ci` | `src/agent_ci`, `src/bin/ci.rs` |

## Rules

- No program talks to another: no RPC, no new port, no shared process state. They coordinate only through the shared store (and GitHub or GitLab).
- No program names another program's module. What two programs both need lives in a neutral module (`text`, `repo_id`, `ci_logs`, `docker_probe`, `low_priority`, `ci_label`, and the rest of the shared plumbing: `backlog`, `link`, `env`, `config`).
- A program that cannot reach the store does nothing and says so. There is no local fallback.

`src/program_boundaries_tests.rs` enforces the module rule by scanning the source for `crate::` and `retaskable_agent::` paths: `chat` and `bin/chat_agent.rs` may not name `orchestrator` or `agent_ci`; `agent_ci` and `bin/ci.rs` may not name `orchestrator` or `chat`; every other file may not name `chat` or `agent_ci`.

## Known gap: the Chat Agent's projects

The Chat Agent still reads its project list (`projects[]`: repo, base branch) from the code agent's `config.json`, or names one repository with `RETASKABLE_CHAT_REPO_ID`. That is the one thing tying it to the code agent's data folder. Splitting it needs a way to learn the projects from the store it is linked to. The store has no such op today: every `role: "chat"` op is per repository and needs the repository id first (chat-agent-process.md section 5), and `/agents` is a signed-in web page. The store change needed is a read for a chat link that returns the projects the person's linked agent may serve, each with its repository id, `githubRepo` and base branch.
