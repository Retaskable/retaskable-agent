# The agent in Docker

**Easiest: the one install, [docker/README.md](../README.md)**: the agent and the CI runner from one compose file, set up by
a wizard on the settings page, with no variable or terminal step. This file is for running the agent alone.

Runs this agent in a Linux container instead of on Windows itself: every process it starts (git,
checks, Claude Code) is much faster there, since Windows Defender scans each one. See
[docs/specs/docker.md](../../docs/specs/docker.md) for why and how this is built.

The container is `retaskable-agent-docker` (compose project `retaskable-agent-docker`), in `runs` mode, and **starts
paused**: nothing is claimed until you resume it from the site or the settings page. Its data folder is the `retaskable-data`
volume at `/data` (shared with the CI runner, `docker/ci/compose.yaml`, so this page manages both: a tab each; never
`docker compose down -v`), with the key for saved secrets in it (`/data/secrets.key`, made on the first save). No token is
passed in the environment.

## Use the published image

Each release publishes `ghcr.io/retaskable/retaskable-agent` (linux/amd64 and linux/arm64; no secret is in it). No
checkout is needed:

```sh
curl -fLO https://raw.githubusercontent.com/Retaskable/retaskable-agent/main/docker/agent/compose.yaml
# export RETASKABLE_VERSION=0.1.1   # a release's version, without the v; default is latest
docker compose pull
docker compose run --rm -T agent retaskable secret set GH_TOKEN     # step 2 below, before the first start
docker compose up -d
```

Then follow "Set up" from step 2 (saving secrets, linking, the settings page), using plain `docker compose` here in
place of `docker compose -f docker/agent/compose.yaml`.

## Build from source

From a checkout of this repo, at its root, add `--build` (or run `docker compose -f docker/agent/compose.yaml build`):
`docker compose -f docker/agent/compose.yaml up -d --build`. The commands below are written this way.

## Set up

1. Optionally put this in a `.env` file beside this `README.md` (never committed): `RETASKABLE_MACHINE=jcnash-docker`.
   Git's name and email are not set here: the agent takes them from the linked GitHub account.

2. Build it (or `docker compose -f docker/agent/compose.yaml pull` for the published image), and save its secrets before it first starts (each asks with no echo, or reads a line from stdin; never
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

A newer agent is a newer image (there's no self-update here yet, unlike the Windows build): `docker compose pull`
then `docker compose up -d` for the published image, or, from source, pull this repo's `main` and
`docker compose -f docker/agent/compose.yaml up -d --build` again. The volumes carry your projects, links, saved secrets and sign-ins across the rebuild.

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

## Disk space

On Docker Desktop (Windows and macOS) every container, image and volume lives in one virtual disk, the WSL2
`docker_data.vhdx` or Docker Desktop's `Docker.raw`. It grows as Docker uses space but does not shrink when Docker
frees it, and a disk that fills completely can corrupt Docker's data, **the `data` volume (links, saved secrets,
events) included**. Look at it now and then:

```sh
docker system df        # images, containers, volumes and build cache, with what is reclaimable
docker system df -v     # each volume's size, so `work` (checkouts, `target/`, `node_modules/`) stands out
```

The agent looks after its own part ([docs/specs/docker.md](../../docs/specs/docker.md) section 10). A free lane that
has sat idle for 24 hours (`RETASKABLE_LANE_IDLE_HOURS`) loses its `target/` and `node_modules/`; session transcripts
are pruned by age and size; and every 5 minutes it reads the free space of `/data` and `/work`. Below 10 GB
(`RETASKABLE_DISK_MIN_FREE_GB`) it takes no new work, reports `Disk nearly full: N GB free on <path>; cleaning up,
taking no new work` to the board and cleans up at once; it resumes at 15 GB (`RETASKABLE_DISK_RESUME_MARGIN_GB`, 5 by
default). It deletes only what it made. It cannot shrink Docker Desktop's disk image, so keep room on the host drive
and consider a limit in Docker Desktop's Settings, Resources, Virtual disk limit.
