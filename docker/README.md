# The one install

`docker/compose.yaml` runs the agent and the CI runner together, from the published images, with no variable to set.
The spec is [docs/specs/setup-wizard.md](../docs/specs/setup-wizard.md).

```sh
docker compose -f docker/compose.yaml up -d          # from a checkout; or download compose.yaml and drop the -f path
```

Then open <http://127.0.0.1:4710>. The settings page opens on a **Setup** wizard whenever a step is missing (a first
start, or after the data volume was wiped), and signs you in itself: no log link is needed until setup is complete.
Each step checks itself and says what is done, what needs you and what failed and why:

1. **Link to retaskable.com**: one button; approve the code on the site. The shared backlog and the CI runner each ask for
   one approval too, and the page starts the next by itself.
2. **GitHub token**: *Create a token* opens GitHub with the scopes (`repo`, `workflow`, `read:org`) filled in; paste it. It is
   saved encrypted, read back from disk and checked with `gh api user`. The runner uses the same one.
3. **Claude**: paste the token `claude setup-token` prints (or an API key). It is checked with `claude auth status`;
   *Test it* sends one tiny message.
4. **Projects**: tick your GitHub repos; each is cloned here and starts paused.
5. **Approve commands**: a project's new commands wait for you on retaskable.com.
6. **CI**: shows the runner, its link, token, Docker and last check; *Turn CI off* if you don't want it here.

Git's name and email come from the GitHub account. Nothing secret is in the environment or the compose file.

## What it starts

| Container | Is | Page |
|---|---|---|
| `retaskable-agent-docker` | the code agent | `http://127.0.0.1:4710` (`RETASKABLE_PORT`) |
| `retaskable-ci` | the CI runner, its own program | `http://127.0.0.1:4702` (also a tab on the agent's page) |

Both mount the `retaskable-data` volume at `/data`: the runner takes the agent's GitHub token and its project list from
it (so one token and one project list cover both), and keeps its own link to the shared backlog.

### The Docker socket

The runner starts check containers through the host's Docker socket, and which socket differs: Docker Desktop on WSL has
`/var/run/docker.sock.raw` (group 0) from the moment Docker is up, a Linux server has `/var/run/docker.sock` (group
`docker`). The runner's entrypoint (`docker/ci/ci-start.sh`) finds the one that exists and its group at every start (the
compose file mounts the host's `/var/run` read-only for it), so nothing is set. If neither exists it says so in
`docker compose logs ci` and the CI step says "Docker missing". `docs/specs/setup-wizard.md` section 2.2 has the reasoning.

## Everyday

```sh
docker compose -f docker/compose.yaml logs -f agent   # also prints the one-time sign-in link
docker compose -f docker/compose.yaml pull && docker compose -f docker/compose.yaml up -d   # update
docker compose -f docker/compose.yaml up -d --build   # from a checkout, build both images from source
docker compose -f docker/compose.yaml down            # stop; volumes stay (never `down -v`: the data volume holds your links)
```

If you ran the two earlier files (`docker/agent`, `docker/ci`), run `docker compose -f docker/ci/compose.yaml down`
first: this file takes the same container names and volumes over, and nothing is lost.

`docker/agent/compose.yaml` (the agent alone) and `docker/ci/compose.yaml` (a server that only runs checks, with its own
`DOCKER_SOCK`, `DOCKER_GID` and `AGENT_CI_NAME`) still work as before.
