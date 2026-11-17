# The agent in Docker

Runs this agent in a Linux container instead of on Windows itself: every process it starts (git,
checks, Claude Code) is much faster there, since Windows Defender scans each one. See
[docs/specs/docker.md](../../docs/specs/docker.md) for why and how this is built.

The container is `retaskable-agent-docker` (compose project `retaskable-agent-docker`), in `runs` mode, and **starts
paused**: nothing is claimed until you resume it from the site or the settings page. Its data folder is the `retaskable-data`
volume at `/data` (shared with the CI runner, `docker/ci/compose.yaml`, so this page manages both: a tab each; never
`docker compose down -v`), with the key for saved secrets in it (`/data/secrets.key`, made on the first save). No token is
passed in the environment.

## Set up

1. Optionally put these in a `.env` file beside this `README.md` (never committed):

   ```sh
   GIT_USER_NAME="Your Name"
   GIT_USER_EMAIL=you@example.com
   # RETASKABLE_MACHINE=jcnash-docker
   ```

2. Build it, and save its secrets before it first starts (each asks with no echo, or reads a line from stdin; never
   put a token on the command line):

   ```sh
   docker compose -f docker/agent/compose.yaml build
   gh auth token | docker compose -f docker/agent/compose.yaml run --rm -T agent retaskable secret set GH_TOKEN
   docker compose -f docker/agent/compose.yaml run --rm agent retaskable secret set CLAUDE_CODE_OAUTH_TOKEN
   docker compose -f docker/agent/compose.yaml run --rm agent retaskable secret list
   ```

   `CLAUDE_CODE_OAUTH_TOKEN` is the long-lived token from `claude setup-token`. `ANTHROPIC_API_KEY`, `GITLAB_TOKEN`,
   `CLOUDFLARE_ACCOUNT_ID` and `CLOUDFLARE_API_TOKEN` are saved the same way when a project needs them. The agent
   puts the saved ones in its environment at start and runs `gh auth setup-git` for a saved `GH_TOKEN`, so git over
   https and `gh` sign in with it. A secret saved while it runs is used at its next start
   (`docker compose -f docker/agent/compose.yaml restart`).

3. Start it and watch its log:

   ```sh
   docker compose -f docker/agent/compose.yaml up -d
   docker compose -f docker/agent/compose.yaml logs -f
   ```

4. Link it the same way as any computer: `docker exec -it retaskable-agent-docker retaskable link [production|staging|local]`
   prints a code and an address to approve; then `retaskable link backlog` for the shared backlog if it doesn't start
   by itself.

5. Open the settings page on the computer running Docker (Windows included): `http://localhost:4700/settings`. The
   log prints the one-time sign-in link, `http://127.0.0.1:4700/settings#<token>`, good once for 5 minutes. It is
   reachable from this computer only: the port is published on `127.0.0.1` only, and the container starts with
   `RETASKABLE_TRUST_DOCKER_HOST=1`, so the page listens on `0.0.0.0` inside it and counts Docker's gateway (the host)
   as this computer ([agent-settings.md](../../docs/specs/agent-settings.md) section 4.4). Never publish the port on another address.

6. Resume it from the site, or from this computer's page, when it is ready to take work. A restart afterwards does
   not pause it again; a project linked later starts paused too (`RETASKABLE_START_PAUSED`, agent-settings.md
   section 3.4).

## Stop it

```sh
docker compose -f docker/agent/compose.yaml stop   # for good, until started again
docker compose -f docker/agent/compose.yaml down    # same, and removes the container (volumes stay)
```

## Update it

Rebuilding is how you get a newer agent (there's no self-update here yet, unlike the Windows
build): pull this repo's `main`, then `docker compose -f docker/agent/compose.yaml up -d --build`
again. The volumes carry your projects, links, saved secrets and sign-ins across the rebuild.

## Where things live

| Volume | Mounted at | Holds |
|---|---|---|
| `data` | `/data` | the data folder: `config.json` (with the saved secrets, encrypted), `secrets.key`, `start-paused.json`, link credentials, events |
| `work` | `/work` | every project's checkout and lane worktrees |
| `claude` | `/home/node/.claude` | Claude Code's settings and sign-in |
| `gh` | `/home/node/.config/gh` | `gh`'s settings |

The key sits in the same volume as the config, so protection there is file permissions, not the cipher
(agent-settings.md section 3.3). Add a project the same way as on any computer: point it at a checkout under `/work`
in `/data/config.json` (`docker exec -it retaskable-agent-docker sh`).
