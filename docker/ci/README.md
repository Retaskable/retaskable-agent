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
GH_TOKEN=$(gh auth token) AGENT_CI_NAME=<computer> CI_RUST_COMMIT=$(git rev-parse HEAD) \
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
