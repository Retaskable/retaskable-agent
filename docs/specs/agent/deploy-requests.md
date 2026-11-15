# Deploy requests — what the computer does

Status: v0.1, draft (2026-11-14). Copies the agent's part of Retaskable.com's [deploy-requests-on-server.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/deploy-requests-on-server.md) (v0.1, backlog item `deploy-requests-claimable`, sections 3.1 and 3.2) and the TypeScript agent's `agent/src/deployRequests.ts`. The store's row and ops (`shared/backlogDeployOps.ts`) are the server's: this file keeps only what the computer sends and runs. Builds on [agent/09.3-what-the-computer-does-when-a-command-asks.md](09.3-what-the-computer-does-when-a-command-asks.md) (the pushed commands these replace), [../rollout-on-server.md](../rollout-on-server.md) (the same shape: the store owns the record, the computer runs and reports) and [../testflight-restart-wait.md](../testflight-restart-wait.md).

Theme: Orchestration

Kind: change

## 1. What it is

The website's TestFlight build, promote, hosting read and go-live prepare are not pushed to one computer. Each is a **deploy request**, a row in the store (`deploy_requests`) that any computer may claim, but only one that can run it. A computer that holds the signing keys or the cloud credentials claims it; the others skip it. The keys stay on the computer and never reach the store: a request holds the command's own arguments and its result only what the code answered.

## 2. What the computer does

- **List** (`deploy_list`): every project's loop reads the open requests every 5 seconds (the store has no push to this computer yet). A request whose status is not `requested` is not touched; a claim that went 10 minutes without a beat already shows `requested` again. A store that can't be reached or read means nothing is claimed (fail closed), and the loop says so once per change.
- **Claim by capability** (`deploy_claim { id, machine, can: true }`): the computer checks its own keys and setup first (`Deps::can`), and calls `deploy_claim` only for a request it can run. It never claims in order to decline on the site's behalf, and the store refuses a claim whose `can` is not `true`. Two computers able to run it: the first claim wins and the other is told so and does nothing. A request whose `args.prefer` names another computer is left to it for 5 seconds from `at`, then taken like any other.
- **Capability per kind** (the code is `deploy_requests::Live::can`):
  - `testflight_build` needs `release.testflight` on and a Mac with the committed Xcode project and `testflight-ios.sh`; the build is Xcode, never EAS. It is claimed only by the computer whose code-host login is the issuer's (case-insensitive) unless `release.testflightAnyMember` is on; a computer that can't say who it is signed in as leaves it to the build's own admin check.
  - `promote` needs the project to have staging and production branches and not merge straight to production.
  - `hosting` and `go_live_prepare` need only the project (its repo and its GitHub name) and a link to the store (the computer holds the backlog token); a paused project still takes them.
  - A paused project (paused here or in the store) claims no `testflight_build` or `promote`.
- **Run**: a claimed request is beaten (`deploy_beat { id }`) at once, which marks it `running`, and every minute while it runs, then run with the code the pushed command ran. A beat that fails is ignored; the store reopens a claim that goes stale.
- **Finish** (`deploy_finish { id, status, result? }`): `done`; `refused` when the code refuses in its own words (the reason is `result.why`, at most 500 characters); `failed` for any other error with the same `why`; a TestFlight build that does not end `submitted` is `failed` with `{ sha, buildNumber, result }`. A result over 4 KB, or with a top-level key that names a secret (`/token|secret|password|credential|api[_-]?key|private/i`, the store's own check, top level only: a nested key passes), is not sent: the request is `failed` with a short `why` instead. A finish from a claim that expired answers `{ ok: false }` and is dropped; the computer says so in its log and does nothing else.
- Several requests run at once, each on its own task, and a request this computer already runs is never claimed twice.

## 2.1 The hosting read

`hosting` is a read: it writes nothing (no store row but its own finish, no file, no git). `src/hosting/` ports `detectHosting` (`agent/src/setup.ts`: the root `package.json` and its npm workspaces' dependencies, a Worker's `main` in `wrangler.toml` or `wrangler.jsonc`, a Dockerfile `EXPOSE`, `flask|django|fastapi` in `requirements.txt`, and the folders `api`, `server`, `functions`, `netlify/functions`, `pages/api`, `app/api`, `src/pages/api`, `src/app/api`, found by path components; Electron, Tauri or `src-tauri` is a desktop app, Expo or React Native a phone app), `gh repo view <repo> --json visibility` (`gh.repoPrivate`: public only when GitHub says `PUBLIC`; `gh` that can't say is not public) and `pickHosting`'s choice (go-live.md section 2).

The result is `{ project, pick }`:

```json
{ "project": { "type": "web", "public": true, "backEnd": false, "app": null },
  "pick": { "shape": "static", "default": "github-pages", "alternatives": ["cloudflare-pages", "netlify"], "database": null } }
```

`pick` names the chosen providers by **id only**: where the TypeScript agent sends each provider's whole object (name, limits, steps, pictures), this sends `default`, `alternatives` and `database` (`{ default, alternatives }` or `null`) as the ids of `shared/hosting.ts`'s `HOSTING` table. The site looks the providers up from its own table ([Retaskable.com#3858](https://github.com/Retaskable/Retaskable.com/issues/3858)), so the agent keeps the ids and the choosing and none of the display text. The result is under 300 bytes for every project, far inside the 4 KB cap. A project with no `projectType` does not exist here (the config's default is `game`), so the TypeScript's "undefined type reads as a static site" has no case.

## 3. What is not here

- The pushed commands are gone from the site; the agent has no case for them in its command handler and no `testflight_report`.
- The `testflight` store log (`log_add`, the logs outbox, pruning of `testflight.log`, `deploys.log` and `rollouts.log`, and `nativeApp` reading the store's `testflight` rows) is the TypeScript spec's section 3.4; it is not ported (section 5).
- The row, the ops, who may request what and the website are the server's.

## 4. Tests

- `src/backlog/deploy/tests.rs`: the ops' wire shapes (a claim always says `can: true`; `finish` sends a result only when there is one; a refusal keeps the store's code; a result over 4 KB is caught before sending; an unknown kind is a bad answer, not a guess).
- `src/deploy_requests/tests.rs`: a request the computer can't run is left unclaimed; one it can run is claimed, run and finished with the derived result; a refusal is `refused` with its reason and any other error `failed`; a result over 4 KB or a reason over 500 characters is cut; losing the claim runs nothing; only `requested` rows are claimed; `prefer` waits 5 seconds for the other computer; a request being run is not claimed twice and a long run beats every minute; an unreadable store claims nothing; no store link means no op is sent.

## 5. Slices

1. **This slice (done):** the store ops and the claim, beat and finish loop, with the capability gate, and nothing ported yet to run: the loop starts for a project only once a kind is in `deploy_requests::PORTED`, so today this computer claims nothing and the website's request waits for a computer that can (the TypeScript agent).
2. **`hosting` (done):** the picker's read (section 2.1), in `PORTED`.
2b. `go_live_prepare`: the item that prepares the repo (`add { via: "site" }`), in `PORTED` with its arm in `Live::run`.
3. `promote`: the staging-to-production fast-forward push with its checks (git, the admin check, the code host's rules), and the deploy log row.
4. `testflight_build`: Xcode build and upload, the `testflight` log kind and its outbox, and `nativeApp` reading the store's rows. The login check above needs the code host's `admins.login`.
5. A push from the store (`facts_changed` naming `deploy_requests`), when the Rust agent has a store link, to replace the 5-second read.
