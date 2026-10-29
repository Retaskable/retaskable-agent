## 10. Config — `.retaskable/config.json`

Written by the agent when a project is linked and when a change to its settings on the site is approved, never edited by hand in normal use ([project-settings-on-server.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/project-settings-on-server.md)). It is version 2: the machine's settings (concurrency, models, the local model, the daily budget, port, cloud and remote) at the top level and each project's in `projects[]`, and a config like the one below (version 1) is read as one project and rewritten on the next save ([projects.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/projects.md) section 3). `githubRepo` is derived from the repo's `origin` remote with `gh repo view --json nameWithOwner`.

```json
{
  "repoPath": "C:/Users/mail/working/Personal/Games/void-miner",
  "projectType": "game",
  "githubRepo": "jamez667/void-miner",
  "backlogPath": "docs/specs/build-order.md",
  "baseBranch": "main",
  "requireCi": false,
  "worktreesDir": "C:/Users/mail/working/Personal/Games/.retaskable-worktrees/void-miner",
  "lanes": 2,
  "concurrency": 1,
  "pollIntervalSec": 10,
  "maxReviewRounds": 3,
  "maxConflictRounds": 5,
  "maxCheckRounds": 3,
  "maxReasks": 2,
  "checks": [
    { "name": "test", "command": "cargo test", "blocking": true, "timeoutSec": 1800 },
    { "name": "verify", "command": "cargo run --release -- --play --verify --headless", "blocking": true, "timeoutSec": 900 },
    { "name": "clippy", "command": "cargo clippy --all-targets", "blocking": false, "timeoutSec": 900 },
    { "name": "fmt", "command": "cargo fmt --check", "blocking": false, "timeoutSec": 120 }
  ],
  "review": { "maxScoutDiffLines": 2000, "minScoutDiffLines": 1000 },
  "codeGates": { "maxFileLines": 1000, "dupLines": 8, "maxSpecChars": 40000 },
  "sandbox": "auto",
  "playCommand": "cargo run --release -- --play",
  "galleries": [
    { "name": "Models", "command": "cargo run --release -- --models --shoot", "outDir": "target/models" },
    { "name": "UI", "command": "", "outDir": "target/ui" }
  ],
  "playtests": "after_merge",
  "chores": "on",
  "triage": "on",
  "maxSlicesPerItem": 10,
  "models": { "opusAbovePct": 50, "sonnetAbovePct": 20, "pauseBelowPct": 5 },
  "budget": { "plannerUsd": 1.5, "implementerUsd": 5, "scoutUsd": 0.3, "reviewerUsd": 1.5, "drafterUsd": 2, "askUsd": 0.5, "releaseNotesUsd": 0.5, "dailyUsd": 0 },
  "port": 4700,
  "local": { "enabled": false, "baseUrl": "http://localhost:11436", "model": "" }
}
```

`budget.releaseNotesUsd` (default `0`, no cap, as every session's since 2026-10-11: on a subscription the usage window is the limit, and a dollar cap only stops a session short) caps each release notes session. Release notes are kept in the shared backlog, never in a folder on this computer ([backlog.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/backlog.md) section 8); an older repo's already-saved notes under `releasesDir` (`"docs/releases"`) are only ever moved from, once ([release-notes.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/release-notes.md) section 9).

(github-workflow) A new setup also writes `github` ([github-workflow.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/github-workflow.md) section 3): the pattern, the protected branch, how a pull request lands, and the rules. A new setup of this repo writes `github_flow` with `baseBranch` and `protectedBranch` both `main`. The example above has no `github` object, which is a project already running: when `baseBranch` is the default branch, that file keeps squash-merging into it, and startup does not add `github` or move the branch. Settings writes the object on the next save.

`ciCoversChecks` (optional, default `false`) skips running `checks` in a lane after CI passes (section 6.3, stage 1). It only applies with `requireCi`, and not with `ciWhenReady`.

`ciWhenReady` (optional, default `false`) runs CI only once a change is ready, not on every push: Retaskable adds the `ci-ready` label to the PR as its implementer's work goes to the lane's checks (CI then runs beside the checks and the review; since 2026-10-14, before only once approved), creating the label in the repo the first time, and takes it off when the item goes back to `implementing` ([ci-when-ready.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/ci-when-ready.md)). It only applies with `requireCi`, and needs the repo's workflow to skip pull requests without the label; one that doesn't still runs on every push, and nothing breaks.

In Retaskable's own repo, `checks` are `npm run typecheck` and the tests. The tests' command should be `npm run test:changed`, which runs only the tests the lane's change can affect (against the merge-base with `origin/main`), and lint and the app's tests only when they're affected; Woodpecker does the same on the pull request and runs the whole `npm test` on every push to `main` ([ci-when-ready.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/ci-when-ready.md) section 6). The human switches the config from `npm test`.

`deploy` (optional) says where the project deploys from ([environments.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/environments.md) section 4.1): `{ "staging": { "branch", "url"?, "smoke"?, "versionPath"? }, "production": { "branch", "url"?, "versionPath"? } }`. A staging `smoke` command turns on the staging smoke after each merge (section 6.3). Production's `versionPath` (default `/version.json`) is how a hosted project learns an item is in production ([backlog.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/backlog.md) section 6.5). Retaskable's own repo uses `{ "staging": { "branch": "main", "url": "https://staging.retaskable.com", "smoke": "npm run smoke:staging -w cloud", "versionPath": "/version.json" }, "production": { "branch": "production", "url": "https://retaskable.com" } }`.

`phone` (optional) configures Play on phone for a project with a Capacitor Android app ([play-on-phone.md](../play-on-phone.md) section 2).

`maxCheckRounds` (default 3) caps how many times failed CI or checks send a slice back, separately from `maxReviewRounds` (section 5).

`maxReasks` (default 2) caps how many times in a row a question the implementer asks again, that a person already answered, is sent back to the session instead of to the person; past it, the item needs a human (section 5, the `needs_clarification` row).

`maxResumeContextTokens` (default 150,000) and `maxTurns` (default 300) are this computer's own settings (`MACHINE_KEYS`, set in its config file; not on the site's settings yet): the size past which an implementer's follow-up starts a fresh session rather than resuming, and Claude Code's `--max-turns` for an implementer session (section 6.2).

`codeGates` (default `{ "maxFileLines": 1000, "dupLines": 8, "maxSpecChars": 40000 }`, a project setting held on the server) sets the code gates before every push (section 6.2): the most lines a touched source file may have, how many meaningful lines an added copy needs to be refused, and the most characters a touched spec may have. 0 turns any one off, and a key left out takes its default.

`projectType` is `game`, `web`, `server`, `cli` or `other`, and a config without it reads as `game` ([project-types.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/project-types.md) section 2).

`engines` (optional, cursor-engine) chooses Claude Code or Cursor for the project and for each group of roles, and its `allow` adds entries to a writing Cursor session's allowlist ([engines.md](../engines.md) sections 3.1 and 6.2). Missing means Claude Code for every role. The site's project settings edit it. The machine's `cursor` (optional) holds Cursor's model per tier, `auto` when missing (engines.md section 3.2).

`galleries` defaults to an empty Models (`target/models`) and UI (`target/ui`). In Retaskable's own repo, `npm run shoot:ui` is the UI gallery's command ([ui-shots.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/ui-shots.md)). Each one's slug (its name in kebab case) names its tab, its folder and its routes, so a gallery named Releases, two whose names make the same slug, or a name whose slug is empty is refused (`galleryErrors` in `shared/galleries.ts`).

The `checks` above are what the scan would suggest for void-miner. `checks` commands run through the shell with the lane as cwd and `RETASKABLE_LANE` (and its old name, [retaskable.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/retaskable.md) section 7.3) set. `worktreesDir` defaults to `<parent of repoPath>/.retaskable-worktrees/<repo name>`. The lanes' folder from before the rename is moved there at start when nothing is running in it ([retaskable.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/retaskable.md) section 7.3). `backlogPath` is relative to the repo root and is stored with forward slashes (it's a git path), while `repoPath` and `worktreesDir` go through `path` APIs.
