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

## Each program has its own settings file

The code agent keeps `config.json` in the data folder, the Chat Agent `chat/settings.json` and the CI runner `ci/settings.json`. Nothing is shared between them but the data folder: no program reads another's file. Each field is layered the same way: the environment first, then the file, then the default; the settings page edits the file only and shows a field the environment sets as locked. The schema and the file I/O of the Chat Agent's and the CI runner's files are in `src/program_settings/` (`chat.rs`, `ci.rs`), neutral modules that the owning program and the page both import, so the boundary test stays green. See agent-ci.md section 3.7.

## The Chat Agent's projects come from the store

The Chat Agent learns its projects from the store it is linked to, not from the code agent's `config.json` (chat-agent-process.md section 5.1, Retaskable.com#3870): `POST <store>/v1/agent/ops` with `{ "op": "projects_served", "args": {} }` and its own store access token, answering `repoId`, `githubRepo`, `defaultBranch` and `access` for each repository. It asks at start and every 5 minutes; a project that appears gets a socket, one the store stops naming loses it.

- **Fail closed.** A refusal (401, 403, a bad answer) or no link clears the list: nothing is served, and the log says why. An unreachable or busy store (429, 5xx) leaves the last list, held in memory only, because every chat op needs that same store. A store that names nothing serves nothing, whatever a `config.json` on the computer says; it is never read.
- **Base branch** is the store's `defaultBranch`. A project that works on a dev branch has its code questions answered against the default branch until the store holds `baseBranch`.
- **`RETASKABLE_CHAT_REPO_ID`** now only limits the list to that one repository id; it names nothing the store didn't.
- **Repo-less projects** (`projectBacklogs`) are ignored: the Chat Agent's board, hello and asks are keyed by a repository.
- **Cloud URL** is `RETASKABLE_CLOUD_URL`, else the build's environment, else retaskable.com; the code agent's `cloud.url` isn't read.
- **The socket** connects as `?project=<kebab of owner/name>` and says `project: { githubRepo }` with no `backlogPath`; the hub places the project by the repository, and only finds one a Coding Agent has already connected (Retaskable.com#3873).
- This needs #3870 and #3873 deployed first.
