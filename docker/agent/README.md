# The agent in Docker

Runs this agent in a Linux container instead of on Windows itself: every process it starts (git,
checks, Claude Code) is much faster there, since Windows Defender scans each one. See
[docs/specs/docker.md](../../docs/specs/docker.md) for why and how this is built.

## Set up

1. Copy the environment variables you need into a `.env` file beside this `README.md` (it is
   never committed — keep your tokens out of git):

   ```sh
   GH_TOKEN=ghp_...
   GIT_USER_NAME="Your Name"
   GIT_USER_EMAIL=you@example.com
   # RETASKABLE_MACHINE=jcnash-docker
   # CLAUDE_CODE_OAUTH_TOKEN=...
   # GITLAB_TOKEN=...
   # RETASKABLE_WOODPECKER_TOKEN=...
   ```

   `GH_TOKEN` is required: it signs `gh` and git-over-https in as you. Leave
   `CLAUDE_CODE_OAUTH_TOKEN` unset to sign Claude Code in by hand instead (see below).

2. Build and start it:

   ```sh
   docker compose -f docker/agent/compose.yaml up -d --build
   ```

3. If you didn't set `CLAUDE_CODE_OAUTH_TOKEN`, sign Claude Code in once:

   ```sh
   docker exec -it retaskable-agent claude
   # then /login
   ```

4. Watch its log:

   ```sh
   docker compose -f docker/agent/compose.yaml logs -f
   ```

5. Link it to retaskable.com and your projects the same way as any computer (agent-settings.md),
   through the settings page the container's `GET /api/health`'s neighbour serves — reach it from
   the host with `docker exec -it retaskable-agent curl http://127.0.0.1:4700/...` or by turning on
   network access (agent-settings.md section 4.4) and publishing the port yourself.

## Stop it

```sh
docker compose -f docker/agent/compose.yaml stop   # for good, until started again
docker compose -f docker/agent/compose.yaml down    # same, and removes the container (volumes stay)
```

## Update it

Rebuilding is how you get a newer agent (there's no self-update here yet, unlike the Windows
build): pull this repo's `main`, then `docker compose -f docker/agent/compose.yaml up -d --build`
again. The `work`, `claude` and `gh` volumes carry your projects, links and sign-ins across the
rebuild.

## Where things live

| Volume | Mounted at | Holds |
|---|---|---|
| `work` | `/work` | `.retaskable/` (its settings and, with no OS credential store here, `credentials.json`), every project's checkout and lane worktrees |
| `claude` | `/home/node/.claude` | Claude Code's settings and sign-in |
| `gh` | `/home/node/.config/gh` | `gh`'s settings |

Add a project the same way as on any computer: point it at a checkout under `/work` in
`/work/.retaskable/config.json` (`docker exec -it retaskable-agent sh`, or mount the file in).
