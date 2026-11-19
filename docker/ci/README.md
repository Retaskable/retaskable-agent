# The CI runner in Docker

`retaskable-ci run` in its own container, checking Retaskable/Retaskable.com and Retaskable/retaskable-agent (set
`AGENT_CI_REPO` to change that). See [docs/specs/agent-ci.md](../../docs/specs/agent-ci.md) section 8.

These files used to live in Retaskable.com's `docker/ci`, which fetched this repo at a pinned `CI_RUST_COMMIT`. Now
the image is built from this checkout itself; the Compose project, the container and the `data` volume keep their
names, but the data volume is now the one shared with the agent, `retaskable-data` (below).

## Use the published image

Each release publishes `ghcr.io/retaskable/retaskable-ci` (linux/amd64 and linux/arm64; no secret is in it).
`RETASKABLE_VERSION` picks a release (e.g. `0.1.1`, no `v`); the default is `latest`. No checkout is needed:

```sh
curl -fLO https://raw.githubusercontent.com/Retaskable/retaskable-agent/main/docker/ci/compose.yaml
export DOCKER_GID=$(stat -c %g /var/run/docker.sock) AGENT_CI_NAME=<this computer>
docker compose pull
docker compose up -d
docker compose run --rm ci retaskable-ci link
```

**Docker Desktop on Windows (WSL):** use Docker Desktop's own socket instead, or the runner won't come back after a
reboot (Docker starts before the WSL distro, whose socket path the container would otherwise mount):

```sh
export DOCKER_SOCK=/var/run/docker.sock.raw DOCKER_GID=0 AGENT_CI_NAME=<this computer>
```

Approving a new version is then choosing its tag: set `RETASKABLE_VERSION` to a release you have read, never `latest`
if you want the runner to stay put. Then save its token ("Its GitHub token" below).

## Build from source

From a checkout, at the repo root, `--build` builds the image instead of pulling it ("Start it, or approve a new
version" below). The commands in the rest of this file are written that way.

## One data volume with the agent

This compose file and `docker/agent/compose.yaml` mount one volume, `retaskable-data`, at `/data`: the agent's
settings page then manages this runner too (a tab each; [docs/specs/docker.md](../../docs/specs/docker.md) section
3.1). The runner runs as uid 1000, the agent's `node`, so each can read what the other makes at mode 0600, and it is
given the Docker socket's group. Set that once in the shell you run compose from (it is required):

```sh
export DOCKER_GID=$(stat -c %g /var/run/docker.sock)
```

The agent's container can read this runner's files in `/data/ci`, including its store link's token; it still never
gets the Docker socket.

## Moving to the shared volume (once, from the old `retaskable-ci_data`)

Only if this runner already ran from the old volume (its link, mirrors and logs). Run from the repo root; nothing here
deletes the old volumes, so it can be undone by starting the old setup again.

```sh
export DOCKER_GID=$(stat -c %g /var/run/docker.sock)
export AGENT_CI_NAME=<computer>
docker compose -f docker/ci/compose.yaml down                 # the runner (old volume stays)
docker compose -f docker/agent/compose.yaml stop              # the agent, so nothing writes while it is copied
docker compose -f docker/ci/compose.yaml create --build       # makes retaskable-data, empty, owned by uid 1000
docker run --rm -v retaskable-ci_data:/from:ro -v retaskable-data:/to debian:bookworm-slim sh -c '
  cp -a /from/. /to/ &&
  mkdir -p /to/ci &&
  { [ ! -f /to/ci-credentials.json ] || mv /to/ci-credentials.json /to/ci/credentials.json; } &&
  chown -R 1000:1000 /to &&
  chmod 600 /to/secrets.key /to/ci/credentials.json 2>/dev/null; true'
docker compose -f docker/ci/compose.yaml up -d --build
docker compose -f docker/agent/compose.yaml up -d
```

The runner's `secrets.key` becomes the shared key. The agent's old volume, `retaskable-agent-docker_data`, holds only
a saved `GH_TOKEN` and its own key (it was never linked), so it is not copied: save the token again,
`docker exec -it retaskable-agent-docker retaskable secret set GH_TOKEN` (the old one was sealed under the old key),
then link the agent as usual. Once both are running and the page shows both tabs, the old volumes can be removed with
`docker volume rm retaskable-ci_data retaskable-agent-docker_data`.

## Start it, or approve a new version

From a checkout of this repo at the commit to run (rebuilding from a commit is how a version is approved: the runner
never picks up newer code on its own):

```sh
git fetch origin && git checkout <commit>   # e.g. origin/main
AGENT_CI_NAME=<computer> CI_RUST_COMMIT=$(git rev-parse HEAD) \
  docker compose -f docker/ci/compose.yaml up -d --build
docker compose -f docker/ci/compose.yaml logs -f
```

`docker inspect retaskable-ci --format '{{ index .Config.Labels "org.retaskable.ci-rust-commit" }}'` says which
commit is running.

## Link it (once per computer)

```sh
docker compose -f docker/ci/compose.yaml run --rm ci retaskable-ci link
```

Approve the printed link, then `docker compose -f docker/ci/compose.yaml restart ci`. The token is kept in the
shared volume, as `/data/ci/credentials.json`. Without the link the runner still checks, but merges nothing and shows nothing on the board.

## Its GitHub token

Save it once, sealed in the shared volume (the key is made as `/data/secrets.key`, mode 0600, on the first save), instead
of passing `GH_TOKEN` in the environment. It asks with no echo, or reads one line from stdin; never put it on the
command line:

```sh
gh auth token | docker compose -f docker/ci/compose.yaml run --rm -T ci retaskable-ci secret set GH_TOKEN
docker compose -f docker/ci/compose.yaml run --rm ci retaskable-ci secret list   # name, source, fingerprint; never a value
```

The running runner takes a saved token up at its next poll. A `GH_TOKEN` in the environment still wins over a saved one.

## Its settings page

`retaskable-ci run --settings-port 4702` serves it, and the compose file publishes it on the host's `127.0.0.1:4702`
only. Open `http://localhost:4702/settings` on the computer running Docker (Windows included). The container is started
with `RETASKABLE_TRUST_DOCKER_HOST=1`, so the page listens on `0.0.0.0` inside it and counts Docker's gateway, which is
the host, as this computer ([docs/specs/agent-settings.md](../../docs/specs/agent-settings.md) section 4.4). The first
sign-in is the one-time link in the log; after it, `docker exec -i retaskable-ci retaskable-ci set-password` (asked twice
with no echo) sets a password that also signs in from the host. Never publish the port on another address.

## Running the CI runner on a server

The CI runner can run on its own on a server, with no code agent beside it. It uses the same `docker/ci/compose.yaml`
as above. Its settings page is on port 4702 and stays on the server's `127.0.0.1`, never published on another
address; reach it from your computer with an SSH tunnel (`ssh -L 4702:127.0.0.1:4702 <user>@<server>`, then open
`http://localhost:4702/settings`). Its GitHub token is saved on the server with `retaskable-ci secret set GH_TOKEN`
(see "Its GitHub token" above).
