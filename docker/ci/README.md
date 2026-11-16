# The CI runner in Docker

`retaskable-ci run` in its own container, checking Retaskable/Retaskable.com and Retaskable/retaskable-agent (set
`AGENT_CI_REPO` to change that). See [docs/specs/agent-ci.md](../../docs/specs/agent-ci.md) section 8.

These files used to live in Retaskable.com's `docker/ci`, which fetched this repo at a pinned `CI_RUST_COMMIT`. Now
the image is built from this checkout itself; the Compose project, the container and the `data` volume keep their
names, so the store link and the mirrors carry over.

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

Approve the printed link, then `docker compose -f docker/ci/compose.yaml restart ci`. The token is kept in the `data`
volume. Without the link the runner still checks, but merges nothing and shows nothing on the board.

## Its GitHub token

Save it once, sealed in the `data` volume (the key is made as `/data/secrets.key`, mode 0600, on the first save), instead
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
