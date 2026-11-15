# Programs status

Groundwork for the shared settings page. The Retaskable agent is three separate programs: the code agent (`retaskable-agent`), the chat agent (`retaskable-chat-agent`) and the CI runner (`retaskable-ci`). They may run on different computers or be installed alone, so none of them talks to another: no RPC, no new port, no command. Coordination is the shared store; each program's own settings are its own file; what a program is doing now is a file it writes.

## 1. The data folder

One rule, `paths::data_dir`, for all three, as the code agent has always had it: `RETASKABLE_DATA_DIR` when set, else a release build's own place in the OS (installers/03 section 3.3), else `.retaskable`. Each program's own folder is under it: the code agent's is the data folder itself, the chat agent's is `chat/`, the CI runner's is `ci/`.

## 2. The status file

Each program writes `<its folder>/status.json` about every 10 seconds while it runs: the temp file `status.json.tmp` in the same folder, then a rename over the old file (tried again for a moment if a reader has the old one open, as on Windows). A reader never sees half a file.

```json
{
  "program": "ci-runner",
  "version": "0.1.0",
  "pid": 4242,
  "startedAt": 1760000000000,
  "updatedAt": 1760000010000,
  "linked": { "store": true, "cloud": false },
  "lastError": "optional, at most 300 characters",
  "envSet": ["AGENT_CI_REPO", "RETASKABLE_DATA_DIR"],
  "docker": "ok",
  "slots": 2,
  "slotsBusy": 1
}
```

- `program` is `code-agent`, `chat-agent` or `ci-runner`. Times are milliseconds since 1970.
- `linked.store` says whether the program holds a link to the shared backlog; `linked.cloud` is left out by a program with no cloud link (the CI runner).
- `envSet` lists the names of the program's settings that an environment variable currently sets. Names only: never a value, a token or a path. The file holds no secret.
- Each program adds only what it already knows: the code agent `port` and `projects`; the chat agent `claudeAvailable` and `projectsServed`; the CI runner `docker` (`ok`, `missing`, `stopped`), `slots` and `slotsBusy`.

## 3. Reading it

`program_status::state_in(folder, now)` gives:

- Running: `updatedAt` is no more than 3 intervals (30 seconds) old and the store is linked.
- Unlinked: written recently, but `linked.store` is false.
- Stopped, with the last time seen: the file is older than that, or the folder has no readable file.
- NotInstalled: no folder and no status file.

The pid is shown beside a state as a hint and never decides one, since a pid can be reused. A program that stops is not told to write a last file: its file goes stale.

Writing and reading live in `program_status`, which imports none of the three programs.
