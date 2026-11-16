# The three programs and where they split

This repository builds three programs. They run together on one computer today and must stay easy to run apart: the CI runner may already be on another server, and the Chat Agent may follow.

| Program | Binary | Code |
| --- | --- | --- |
| Code agent | `retaskable` | everything not listed below, including `src/main.rs` |
| Chat Agent | `retaskable-chat-agent` | `src/chat`, `src/bin/chat_agent.rs` |
| CI runner | `retaskable-ci` | `src/agent_ci`, `src/bin/ci.rs` |

## Rules

- No program talks to another: no RPC, no new port, no shared process state. They coordinate only through the shared store (and GitHub or GitLab).
- No program names another program's module. What two programs both need lives in a neutral module (`text`, `repo_id`, `ci_logs`, `docker_probe`, `low_priority`, `ci_label`, and the rest of the shared plumbing: `backlog`, `link`, `env`, `config`, and `page_host`, the settings page's sign-in, checks and programs panel, which the code agent's page and the CI runner's own page both build on).
- A program that cannot reach the store does nothing and says so. There is no local fallback.
- The programs on one computer share one data folder, each with its own part of it (the code agent's root, `chat/`, `ci/`), and that is the only thing they share besides the store. The code agent's settings page shows a tab for each program installed in that folder, and edits only that program's own `settings.json` and sealed secrets; it never calls the program (agent-settings.md section 4.6). In Docker the agent and the CI runner mount one volume to share the folder (docker.md section 3.1).

The HTTP port of each program that has one serves only `GET /api/health` and a settings page, never a command: the code agent's, and the CI runner's when it hosts its own page (agent-ci.md section 3.9). `src/settings_server/routes_tests.rs` lists the routes of both ports and fails when one is added.

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

## Every store call carries an access token

**[Decided 2026-11-16]**, `src/backlog/access.rs`. The store verifies a signed, short-lived access token as the bearer of every op (backlog devices.ts `verifyAccessToken`) and knows a refresh token only at `POST /v1/device/token`. So the code agent, like the TypeScript agent (link.ts `DeviceTokens`, backlogStore.ts `send`), the Chat Agent and the CI runner, never sends the stored refresh token as a bearer: one `link::DeviceTokens` for the store (`BACKLOG_TARGET`), built in `main.rs`, swaps it for an access token and keeps that until a minute before it runs out. The orchestrator, rollout and deploy requests share it (`AccessTokens`), and `backlog::Client::with_access` sends a call again once with a fresh token when the store answers 401 to the one it carried.

- **Fail closed.** A refused refresh token (`invalid_grant`) is no link: the stored token is dropped, `backlog::unlinked_on_site()` stays set so no new link starts by itself, and every store call is skipped as "this computer holds no link to the backlog store". A store that can't be reached for a token skips the call with that reason; nothing falls back to the refresh token.
- **One refresh at a time.** Refresh tokens rotate on every use, so the lock is held across a refresh; Quit waits up to 5 s for one under way (`DeviceTokens::settled`) so the rotated token is stored. (A restart through `std::process::exit` doesn't wait.)
- The store socket (`backlog_socket`) uses the same `DeviceTokens`. Before this the stored refresh token was sent as the bearer, so every store call from the code agent was a 401 against the real store.
