# Setup wizard and one install — Spec

Status: v0.1, decided (2026-11-20). Builds on [docker.md](docker.md), [agent-settings.md](agent-settings.md) (the page, secrets, the Link buttons, Add project), [first-run-check.md](first-run-check.md), [tray.md](tray.md) (linking), [agent-ci.md](agent-ci.md) and [programs.md](programs.md) (three programs, one page).

Theme: Setup

Kind: new

## 1. What it is

**[Decided 2026-11-20]** The owner, after Docker's data was wiped and setting up again took five separate chores: "this is way too hard, can you make the default install the agent and ci and then it needs a wizard that walks me through each step of what it needs." Half of those chores silently did nothing (a GitHub token saved on the page that never reached a file) and nothing told him.

So there are two changes:

1. **One install.** `docker compose -f docker/compose.yaml up -d` starts the agent and the CI runner from the published images, with no environment variable required (section 2).
2. **A wizard** on the agent's settings page that walks through what the computer needs, checks every step itself, and says in words what is done, what needs the person, and what failed and why (section 3).

Docker Desktop is the one thing a person installs. Everything else is the wizard.

## 2. One install

### 2.1 The compose file

`docker/compose.yaml` has two services, `agent` and `ci`, on the published images (`ghcr.io/retaskable/retaskable-agent` and `ghcr.io/retaskable/retaskable-ci`, `${RETASKABLE_VERSION:-latest}`), each with `build:` so `up -d --build` builds from source. It sets the Compose project name `retaskable-agent-docker`, the container names `retaskable-agent-docker` and `retaskable-ci`, and the shared volume name `retaskable-data`, which are exactly what the earlier two files used, so a computer that already runs them keeps its volumes (its `work`, `claude` and `gh` volumes belong to that project name).

- **No variable is required.** `docker compose config` renders with an empty environment (a test, section 6). Every `${VAR}` has a default; the two old required ones (`DOCKER_GID`, `AGENT_CI_NAME`) are gone (sections 2.2 and 2.4).
- **The page is at `http://127.0.0.1:4710`.** `RETASKABLE_PORT` defaults to 4710 in this file (the old agent file's 4700 stays for `docker/agent/compose.yaml`). The CI runner's own page, 4702, is published on `127.0.0.1` as before.
- **The CI runner is on by default** and the wizard can turn it off (section 3.7). It is a separate container running its own program (programs.md): it shares no process and no port with the agent, only the `retaskable-data` volume.
- `docker/agent/compose.yaml` and `docker/ci/compose.yaml` keep working. The CI one is for a server that runs only the runner; it keeps `DOCKER_SOCK`, `DOCKER_GID` and `AGENT_CI_NAME`. Moving from the two files to one: `docker compose -f docker/ci/compose.yaml down` first (it frees the `retaskable-ci` name; the volume stays), then `up -d` the new file.

### 2.2 The Docker socket, found by the container

**[Decided 2026-11-20]** The runner starts check containers through the host's Docker socket. Which socket and which group owns it differs: Docker Desktop on WSL has `/var/run/docker.sock.raw` (root, group 0) from the moment Docker is up, while `/var/run/docker.sock` there appears only once a WSL shell is open and a bind mount of a missing file makes Docker create a *directory* in its place; a Linux server has only `/var/run/docker.sock` (group `docker`). One bind mount of a fixed file cannot be right for both, and Compose cannot say "mount whichever exists".

So the `ci` service mounts the host's `/var/run` **directory**, read-only, at `/host-run`, and its entrypoint (`docker/ci/ci-start.sh`) decides at every start:

1. `/host-run/docker.sock.raw` if it is a socket, else `/host-run/docker.sock` if it is a socket, else it stops with one line saying no Docker socket was found and what the person can do (start Docker Desktop; on Linux check `/var/run/docker.sock`).
2. The group is the socket's own (`stat -c %g`), so it is 0 on Docker Desktop and the `docker` group's id on Linux, with no `DOCKER_GID` to look up.
3. It makes `/var/run/docker.sock` a link to the choice, then drops from root to uid 1000 (`setpriv --reuid 1000 --regid 1000 --groups <gid>`) and runs the runner. uid 1000 is still the agent's `node`, so what either makes at mode 0600 in the shared volume stays readable to both (docker.md section 6).

`DOCKER_SOCKET_DRY_RUN=1` makes the script print `socket=<path> gid=<n>` and exit, which is what the socket-detection tests run over a temporary directory of sockets.

**The trade.** The runner container can see the host's `/var/run` (read-only, but a socket can be connected to). The runner already holds the Docker socket, which is root on the host, so this adds sockets of the same kind (containerd, a desktop's own); it is why only the runner, never the agent, gets it, and the runner is the program's own code, not a project's check (those run in their own containers with no token and none of this). A person who wants the narrower mount uses `docker/ci/compose.yaml` and sets `DOCKER_SOCK` and `DOCKER_GID`.

### 2.3 Where each program gets its setup

- **The agent** is linked and given its tokens by the wizard (section 3), not by an environment variable or a terminal.
- **The git identity** (`user.name`, `user.email`) comes from the linked GitHub account, never an environment variable: when the GitHub token is saved or checked, and at each start with a saved token, the agent reads `gh api user` and, only where git has no identity yet, sets the global `user.name` (the account's name, else its login) and `user.email` (its public email, else the `<id>+<login>@users.noreply.github.com` address GitHub gives every account). `GIT_USER_NAME` and `GIT_USER_EMAIL` are not read by the compose file or `start.sh`.
- **The CI runner takes its setup from the shared data.**
  - *GitHub token.* `GH_TOKEN` saved for the runner (`/data/ci/settings.json`) wins as before; **when the runner has none of its own, it uses the one the agent saved** (`/data/config.json`, sealed under the same `/data/secrets.key`). Setting it once in the wizard covers both. The page shows the runner's source as "the code agent's".
  - *Link.* The store's role-`ci` token is its own credential, so the code agent's link does not cover it. The wizard links the runner for the person as part of its link step (section 3.1): the agent starts the runner's device flow (`program_link`, the one the runner's `retaskable-ci link` runs) as soon as the agent's own links are done, writing the token where the runner reads it (`/data/ci/credentials.json`; the agent's compose service sets `RETASKABLE_CI_CREDENTIALS_FILE` to it, because the page refuses to link a program whose environment names a credentials file it cannot see), and the runner picks it up with no restart (`link::DeviceTokens` reads its store on each refresh).
  - *Name.* The runner's name is the computer's: with no `AGENT_CI_NAME` it uses the agent's `RETASKABLE_MACHINE` (compose sets both to `retaskable-agent-docker`); `AGENT_CI_NAME` still overrides.
  - *Repos.* With `RETASKABLE_CI_FROM_AGENT=1` (the one compose file sets it) and no `AGENT_CI_REPO` or repos of its own, the runner checks the repos of the agent's projects: it reads the project list from the shared data (`/data/config.json`'s `githubRepo` and `baseBranch`) at every poll, so a project linked in the wizard is checked within seconds, and a computer with no project yet idles rather than stopping (without the variable a runner with no repo still refuses to start). The compose file no longer names Retaskable's repos, and sets no `AGENT_CI_SLOTS`, so the wizard's CI switch is not locked by an environment variable.

  The runner stays its own program: it reads files in the shared folder and nothing else of the agent's; it makes no call to the agent (programs.md).

## 3. The wizard

### 3.1 When it shows, and how a person gets in

- The wizard is the settings page's first view while **any required step is not done**, including after a wipe (the data volume is empty, so every check fails). With everything done the page opens as before and the wizard is a **Setup** tab that shows the checklist.
- **No log link is needed.** In a container with `RETASKABLE_TRUST_DOCKER_HOST=1` (the compose file sets it; the port is published on the host's `127.0.0.1` only, docker.md section 7), a visit from the host with no sign-in signs the person in by itself **while any required step is not done**: the page asks `POST /api/settings/session` with `{ "firstVisit": true }`, which the server accepts only from the trusted Docker gateway and only while the wizard is incomplete. Once the wizard is complete the door shuts and the one-time link or the password is needed again. The one-time link is still printed at every start. A restart in the middle (Add project restarts the agent) signs nobody out for good, because the door is still open until the last step.
- **The trade.** Until it is complete, any process on the computer that can reach `127.0.0.1:4710` can use the page, as it could use a printed link it read from `docker logs`. The Host and Origin checks still keep a web page in the person's browser out (no DNS rebinding, no cross-site POST). A native install is unchanged: the tray's link, or the one the first run opens, with no first-visit door.
- Where the page says it needs a link (a native install, or the wizard already complete) it says exactly where it is: "In a terminal run `docker compose logs agent` and open the address after 'Settings page, open it once with'".

### 3.2 What the wizard may do

Nothing the settings page may not already do (agent-settings.md): save settings and secrets, start a link, and Add project. **It adds no command and no port.** Its routes are `GET /api/wizard` and the saves `POST /api/wizard/github`, `/claude` (a secret, as `POST /api/settings` saves one, then checked), `/ci` (the runner's slots, a setting as the programs panel's save is) and `/claude/test` (the one fixed probe above); `routes_tests.rs` lists them. Every request has the session, CSRF, Host and Origin checks, and a change is from this computer only. What a project's link and the owner's identity need from retaskable.com goes through its existing API as the linked person, never a local command (section 3.5).

### 3.3 A step

`GET /api/wizard` answers `{ complete, current, steps: [...] }`, each step `{ id, title, state, detail, todo?, failed?, data? }`:

- **state** is `done`, `needs` (needs the person: `todo` says exactly what to do) or `failed` (`failed` says why, in words, never a token).
- A step is checked from facts on this computer or from a real call, each time the page asks (the page polls every 3 seconds while a step waits); a slow check (GitHub, Claude) is kept for 5 minutes unless the person asks again.
- **A step can be passed only when its check passes**: the wizard's Next is disabled until the step is `done`, and the later steps stay shown but unreached. A step the person can skip says so on its own (CI, section 3.7).

### 3.4 The steps

1. **Link to retaskable.com.** One button, **Link this computer**. It starts the device link (agent-settings.md section 4.6's `POST /api/link/{target}`: retaskable.com, then the shared backlog store, as the Link buttons do) and shows the code and the approval link, and then the runner's (section 2.3); the page polls until each is linked. `done` when this computer holds a token for its cloud and the store **and** the CI runner holds its own (unless CI is off). The page says how many approvals are left ("2 of 3: the runner"), since the store asks for one per credential.
2. **GitHub token.** **Create a token** opens `https://github.com/settings/tokens/new?scopes=repo,workflow,read:org&description=Retaskable%20(<computer>)` (a classic token with `repo`, `workflow`, `read:org`: what `gh`, git pushes, pull requests, checks and the runner's statuses use). The person pastes it; the wizard saves it encrypted (the secrets store) and checks it. `done` when **all** hold: the token reads back from disk (a second `Secrets` opens the config and decrypts it to the same fingerprint), `gh api -i user` answers with a login, the token's scopes (the `X-OAuth-Scopes` header) include `repo` and `workflow` (a fine-grained token has none to read, so it is checked by the next point only), and `gh api user/repos?per_page=1` answers. The step shows the account's name and login. A failure names the stage: "couldn't write /data/config.json: permission denied", "no key to seal with", "GitHub refused the token (HTTP 401)", "the token lacks `workflow`". A save is refused from another computer with 403 and the page says so.
3. **Claude.** Explains in one line how to get a token: "In a terminal on any computer with Claude Code, run `claude setup-token`, sign in, and paste the token it prints (it starts `sk-ant-oat`); or paste an API key instead." The value is saved encrypted as `CLAUDE_CODE_OAUTH_TOKEN` (or `ANTHROPIC_API_KEY`), put in the agent's environment for new sessions as saving any secret does, and checked: `claude auth status` for a signed-in account, and **Test it** runs one `claude -p "Reply with the single word ok." --model haiku` call (a few tokens): a fixed probe like the setup checks' own, with nothing in the request saying what runs. `done` when the status says signed in (a saved token or key counts only once that check passes). A sign-in made another way (a `.credentials.json` in the `claude` volume) passes the same check.
4. **Projects.** The person's GitHub repos (the Add project picker: `/api/projects/owners` and `/repos`), with the ones already on this computer marked, and ticking one links it: Add project (`add_project`), which is the very change the site's **Link project…** makes through `link_project` (clone, the project's entry, started paused), one at a time with the clone's step shown and the agent's restart waited out; the page keeps the ticked repos and takes the next after each restart. `done` when at least one project is on this computer. Listing the person's *retaskable.com* projects instead needs an endpoint the hub does not have yet (section 3.5).
5. **Approve commands.** Each project's commands that wait for approval (`CommandGates::waiting`) in the words the site shows ("checks: `npm test` → `npm run e2e`"), with **Open on retaskable.com** to approve them. `done` when none wait. **[Decided 2026-11-20]** Approving stays on the site, as the owner's act: the command gate exists so that a command is run only after the owner approved it as themselves on retaskable.com, and a local button would turn "someone who can reach this page" into the owner.
6. **CI.** Shows the runner: running, linked (its own link), its GitHub token and where it comes from, its Docker socket (`docker` in its `status.json`: `ok`, `missing`, `stopped`) and its last check (from its `status.json` details). `done` when it is running, linked, has a token and has Docker. **Turn CI off** sets the runner's slots to 0 in its settings (agent-settings.md, "CI slots 0 turns CI off") and the step is `done` as "Off: you turned it off here"; **Turn CI on** puts the default back. There is no test-check button: a check starts only from a real pull request, and the step shows the next real check's repo instead.
7. **Done.** A checklist of every step, all green, with a link to the Now tab.

### 3.5 Reading the person's projects from retaskable.com

**[Not built, needs Retaskable.com]** Listing the owner's retaskable.com projects would be `GET /api/agents/projects` with the agent's access token, answering `{ projects: [{ id, name, githubRepo, organisationSlug? }] }`. It does not exist, and it must go through the connector's own `DeviceTokens`: a second token holder for the same link would refresh the rotating refresh token alongside it and unlink the agent. Until it does, the wizard lists GitHub repos (step 4), which is where the site's own projects come from anyway, and linking is the same either way. The project is announced to retaskable.com by the agent's `hello` once it is added, as with `link_project`.

## 4. Why a saved token did not take (2026-11-19)

The owner saved `GH_TOKEN` on the page and no `config.json` or `secrets.key` appeared. The save path itself (`Secrets::plan` then `commit`, through `POST /api/settings`) is correct and now has an end-to-end test (section 6): from an empty data folder it creates `secrets.key` (0600) and `config.json`, and a fresh process reads the token back. So the failure was not in the cipher; it was that **nothing made a refusal or a failed write visible**: the Secrets card is read-only (and says so in grey, in a card on the Settings tab) whenever the page does not count the request as this computer or has no key to seal with, and a write error shows as one line in the status bar that clears on the next poll. The wizard's token step saves through its own endpoint that reports the stage that stopped it (section 3.4, step 2) and reads the token back from disk, and the wizard's first line says when the data folder is not writable ("`/data` is not writable by this program: …"), the usual cause of a volume that was created by something else first.

## 5. Needs a change on Retaskable.com

- **`GET /api/agents/projects`** for an agent's access token: the projects of the person who linked it (id, name, `githubRepo`, `organisationSlug`), so the wizard can list the retaskable.com projects and not only GitHub repos (section 3.5).
- **Linking the runner with no second approval.** The store asks the person to approve each role's device code. A delegation (an agent token minting a `role: ci` token for the same computer and person, `POST /api/device/delegate`) would remove the runner's approval from section 3.4 step 1.
- Neither blocks this agent: both degrade to what is built.

## 6. Tests

- `src/settings_server/wizard/tests.rs`: each step's check logic with the probes stubbed (a missing link, a waiting one, a done one; a token that fails to write, to open, GitHub's refusal, a missing scope, a good one with the account's name; Claude unset, saved but signed out, signed in; no projects, one; commands waiting; the runner unlinked, without a token, without Docker, running, off); the first-visit door (open only from the trusted gateway and only while incomplete); the secret save and read round trip over HTTP from an empty folder (key and config created, a second `Secrets` reads it; refused from another address; a read-only folder says why).
- `src/secrets/ci_shared_tests.rs`: the CI runner uses the code agent's `GH_TOKEN` from the shared data when it has none of its own, its own wins, and removing the agent's takes it away.
- `src/git_identity.rs`: the name and email from `gh api user` JSON, the no-reply fallback, an existing identity left alone.
- `tests/docker_install.rs`: socket detection over a directory of sockets (`.raw` wins, only `docker.sock`, none, the group follows the socket), and `docker compose config` renders `docker/compose.yaml` with an empty environment (skipped when Docker is not installed).
- The page, in headless Playwright against a throwaway build on another port: a first visit signs in with no link, each step's states render, and Next stays disabled until a step passes.

## 7. Acceptance

- On a fresh machine with Docker Desktop: `docker compose -f docker/compose.yaml up -d`, open `http://127.0.0.1:4710`, and the wizard is the page. Following it to the end links the computer and the runner, saves the GitHub and Claude tokens, links a project and leaves a checklist all green, with no variable set and no terminal beyond the one command (and `claude setup-token`, wherever Claude Code is).
- Removing the `retaskable-data` volume and restarting shows the wizard again, with each step `needs`.
- A save the page cannot make says why, in the wizard, in words.
