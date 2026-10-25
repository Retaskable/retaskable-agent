## 8. What stays

- **The checkout way** (`npm start`, `scripts/supervise.mjs`, self-update by fast-forward) stays for developing the agent and for docker.md's container. A checkout has no `build.json`, so it isn't a release build and links to any environment.
- **The agent's behaviour** doesn't change: same orchestrator, same commands, same 127.0.0.1-only port (CLAUDE.md).

### 8.1 Both builds on one computer

The people building Retaskable will run Production and Staging side by side. Nothing may collide:

- **Folders**: section 3.3's two.
- **Credentials**: the keyring entries are already keyed by each cloud's URL; the store link is shared by design (one store, [backlog.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/backlog.md) section 3.1), so both agents may use one link.
- **Port**: the health check's 4700 (agent.md section 8) is taken by whichever starts first. Staging defaults to 4701.
- **Names and icons**: "Retaskable Staging" everywhere it names itself, and its icon gets a small "S".
- **Projects**: one project set up on both would have two agents on it. That is allowed (the store hands each item to one), but Getting started warns when the same repo is already set up on this computer by the other build.

