# What was trimmed from the agent specs

Status: a snapshot, taken with the specs ([README.md](README.md)). This is the to-do list for thinning the TypeScript agent: each row is something Retaskable.com's agent specs say the agent does, which the store, the hub or the app should do instead. The copies in this folder no longer say it; where there was something to ask for instead, they say what the agent asks the store or hub for.

Where it belongs:

- **store op**: an operation of the shared backlog store ([backlog.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/backlog.md) section 6). The agent asks for it and reports raw results.
- **hub**: the cloud hub (Worker and Durable Objects, [cloud.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/cloud.md)): commands, notifications, routing, anything the server answers without a computer.
- **app**: the Expo app ([expo.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/expo.md)), which is the only UI.

Count by destination (the first one named in each row): store op 35, hub 23, app 64. Total 122.

Store op names are the ones Retaskable.com's specs use. A few (for example do_next, 	ryout_groups) were inferred from the text and need checking against the store before the agent is thinned.

| Spec | Section | What was there | Belongs |
| --- | --- | --- | --- |
| agent-settings.md | 2.1 The hub and the old ways | Owner-only check against D1 `agents.user_id`, `onlyOwnerLine`, MCP exclusion, `retiredRefusal` words, deploy order | hub: cloud agentHub (cloud.md section 6.1) |
| agent-settings.md | 2 (lanes and website switches paragraph) | Lanes and `remote` still asked as `settings_request` until plan PR 6 | hub: cloud commands (settings_request) |
| agent-settings.md | 6 Tested (site part) | `cloud/test/agentSettings.test.ts`, `threatModel.test.ts`, and the app's text tests | hub: cloud tests; app: Expo app tests |
| agent-settings.md | 2.2 The Agents section on the site | Agents page: intro text, per-computer cards, owner form, read-only rows with how to change, secrets and links, Sample's made-up computer, Project settings losing "Its computer" | app: Expo app Settings -> Agents |
| agent-telemetry.md | 4.1 On the screens; 6 Tests | The Agents screen's "What looks wrong" list, the item page's warning line and their tests (`anomalyText`) | app: Expo app Agents screen and item page |
| agent/01-what-it-is.md | whole | Product pitch with the UI, the bell and the local-mode framing | app: product overview; the agent keeps only what runs on the computer |
| agent/02-principles.md | principles | UI principles and no-store fallbacks | app: UI principles; store op: fail closed when the store is unreachable |
| agent/03-scope-v1.md | scope | Project switcher, environments table, UI scope lists | app: project switcher and environments; hub: project commands |
| agent/04-architecture.md | slices data model | Slice records kept locally | store op: items and slices (`claim`, `report`, `complete`) |
| agent/04-architecture.md | Play main UI | Play main buttons and labels | app: Play main controls |
| agent/05-item-lifecycle.md | queued, awaiting_confirm | Queue and confirm states and card wording | store op: item state and queue order (`claim_next`) |
| agent/05-item-lifecycle.md | slice records | Local slice records | store op: `report` |
| agent/06.2-implementer.md | UI wording | Card wording, bell and board text around waiting for steps and questions | app: item cards and bell |
| agent/06.3-review.md | UI wording | Board and card wording for review rounds | app: item cards |
| agent/06.4-playtests.md | grouped into sessions | How the grouping is shown | app: try-out grouping display (store op: `tryout_groups`) |
| agent/06.4-playtests.md | bell entry | The bell item, Needs you, counts and page title for a playtest | app: playtest display |
| agent/06.4-playtests.md | after-merge try-outs | The bell's list of waiting try-outs | store op: `tryout_add`, `tryout_result` |
| agent/06.5-spec-drafts.md | whole | Local draft list, order, UI buttons and file rendering | store op: `draft_claim_next`, `draft_front`, `draft_report`, `draft_answer`; hub: `draft_files` |
| agent/06.6-ask-claude.md | whole | Local chat history, audiences list, clearing, conversation layout | store op: `asks_list`, `ask_report`, `asks_clear`, `facts_get`; app: chat windows |
| agent/06.7-chores-never-idle.md | what kind next | Rotation, day count and limit decided locally | store op: `chore_next`, `chore_lock`, `chores_get` |
| agent/06.7-chores-never-idle.md | chores per day | The count display and Chores tab list | app: Chores tab |
| agent/06.7-chores-never-idle.md | bell notices | Bell lines for upcoming runs and Sync now | app: notices |
| agent/06.7-chores-never-idle.md | scan view | The view of statuses computed from probes | store op: scan view computed from raw per-check results |
| agent/06.8-galleries-the-game-photographs-itself.md | HTTP statuses | 400, 404 and 409 refusals of the shoot request | hub: `shoot` command |
| agent/06.8-galleries-the-game-photographs-itself.md | tab and feedback UI | Setup tab, Add a way to take these, feedback box and picture viewer | app: gallery tab and feedback boxes |
| agent/06.8-galleries-the-game-photographs-itself.md | gallery reads | `gallery_index` and `gallery_image` reading local disk | hub: `gallery_index`, `gallery_image` |
| agent/06.8-galleries-the-game-photographs-itself.md | do next | Loop marks the merged item do next | store op: `do_next` |
| agent/06.9-triage-claude-looks-at-needs-you-first.md | It can't loop (restart durability) | How a `human`/`person_steps` finding and the failed-look count survive a restart: write-through timing against the item's lease and park state, and hydrating a finding that beat the store's own answer back | store op: item details flush and hydrate across a restart (item-details-on-server.md, last-local-state.md) |
| agent/06.9-triage-claude-looks-at-needs-you-first.md | claudeOnIt | UI treatment of an item Claude is looking into | app: under-way display |
| agent/06.9-triage-claude-looks-at-needs-you-first.md | card and bell | Suggested button on the card, bell and Needs you visibility | app: Needs you and item cards |
| agent/07-orchestrator-loop.md | 7.1 Auto concurrency | The Agents tab status light's tooltip with the reason for the number | app: Expo app Agents screen |
| agent/07-orchestrator-loop.md | 7.2 Local model | Setup's advanced toggle, URL and model-name form | app: Expo app / site project settings (agent-settings for the machine's own) |
| agent/07-orchestrator-loop.md | 7.3 Every button works from any computer | Which computer may take over a parked item with which button | hub: cloud commands and store claim rules (claim-while-working.md) |
| agent/07-orchestrator-loop.md | 7 The fetch | The top strip's sync button, its Syncing / Out of sync states and the account menu's sync rows | app: Expo app status strip and account menu |
| agent/07-orchestrator-loop.md | 7 Do next (priority) | How "do next" moves an item to the front and up the spec-draft queue | store op: claim_next ordering and the spec queue (spec-queue.md) |
| agent/07-orchestrator-loop.md | 7 Orchestrator loop (pseudocode) | The loop started "the first queued item whose dependsOn items are all done, or a chore" itself | store op: claim_next (the store picks the item or chore; the agent starts what it hands back) |
| agent/07-orchestrator-loop.md | 7 Usage, not dollars | The Agents tab's usage meters and the header's usage warning | app: Expo app Agents screen and header (the agent only reports usage in the snapshot) |
| agent/08-http-api.md | 8 HTTP API | The history of about 90 `/api/*` routes and the old local UI, the PR "Requests" flow | hub: cloud commands (cloud.md section 6) |
| agent/09-web-ui-2.md | 9 Flow | The List / Flow switch and live diagram | app: Expo app Agents screen (live-flow.md) |
| agent/09-web-ui-2.md | 9 Profile | Profile button, menu, dialog, `GET /api/profile`, link status dialogs | app: Expo app account menu and Profile |
| agent/09-web-ui-2.md | 9 Bell | The bell's entries, badge, seen keys, dropdown, All-agents bell on the site, `shared/inboxEvents.ts` event list | hub: notifications (cloud/src/notifications.ts, shared/inboxEvents.ts); app: bell |
| agent/09-web-ui-2.md | 9 Usage | Usage block, header warning thresholds, wording, phone widths | app: Expo app Agents screen and header |
| agent/09-web-ui-2.md | 9 Chat | The chat expander, conversations, See previous, New conversation, Clear, phone sheet, `POST /api/asks` | app: Expo app chat; store op: asks_list (asks and replies) |
| agent/09-web-ui.md | 9 Release notes | Range pickers, which PRs and entries may be shown, Preview/Source, Save to the backlog, Publish and Unpublish, public page | store op: releases_get, release_set; hub: publish command; app: Releases screen |
| agent/09-web-ui.md | 9 Ideas | `ideas.json` as a local cache and the `/api/ideas*` routes | store op: ideas_list (ideas queue, rounds and answers) |
| agent/09-web-ui.md | 9 Web UI (intro and header) | The dashboard header: status stat, project switcher, Actions menu, bell, profile button, sync chip, words-on-screen table | app: Expo app header and screens |
| agent/09-web-ui.md | 9 Setup form (items 1 to 10) | The Setup screen: repo picker, project type, backlog file, GitHub workflow, Wait for CI toggles, sandbox, checks, galleries, deploys, budgets | app: Expo app / site project settings (project-settings-on-server.md) |
| agent/09-web-ui.md | 9 Tabs: Agents, Releases, galleries | Panel layout, Pause/Resume/Restart buttons, Releases tab, gallery grid and viewer | app: Expo app Agents, Releases and gallery screens |
| agent/09-web-ui.md | 9 Pause and Resume | Button layout, header status wording, status light, narrow widths, restart button texts | app: Expo app Agents screen; hub: pause/resume/restart commands |
| agent/09-web-ui.md | 9 Tabs: Roadmap | The kanban board, swimlanes, columns, cards, drawer, Hide done, Hide tasks not added locally, merge control | app: Expo app Roadmap (roadmap-board.md) |
| agent/09-web-ui.md | 9 Tabs: Inbox (retired) | Needs you, Questions, Spec drafts, Playtests and Requests cards, and the filter tabs, counts and persistence | app: Expo app board and bell (retire-inbox.md) |
| agent/10-config-retaskable-config-json.md | 10 Config | The Setup form wording, the "Shared by all projects" section, `releasesDir` as a setting, and the "Release notes folder" note | app: Expo app / site project settings |
| agent/10-config-retaskable-config-json.md | 10 Config (`allowedHosts`, the Sample's server) | The retired `allowedHosts` key, and `localSite.ts` serving an app build on port 5173 for `npm run launch` | app: Expo app tooling (`npm run launch`), not the agent's port |
| agent/11-build-milestones.md | 11 Build milestones | The planner / Setup / Web UI / API + SSE milestones (retired) | app: Expo app; store op: the shared backlog replaced the planner |
| agent/12-open-questions.md | 12 Open questions | "Open PR to main as a retaskable.com command" and the planner's `dependsOn` stability question | hub: cloud commands (cloud.md section 6.1); store op: planner retired |
| claude-signed-out.md | 3.4 Telling the person | The banner (`ClaudeSignedOutBanner`), the bell line, the card's waiting words, the Sample's signed-out computer and the User Manual entry (`app/src/ui/StatusStrip.tsx`, `bellText.ts`, `cardState.ts`, `shared/demo/*`) | app: Expo app status strip, bell, cards and Sample |
| claude-signed-out.md | 3.4 / 2 Decisions: showing the computer offline and telling its owner | Showing a held computer offline, and the sign-in button's wiring | hub: online/offline and routing, `tool_sign_in` command ([signed-out-offline.md]) |
| claude-signed-out.md | 3.2 / 2 Decisions: "on a shared backlog" give-back | The local-versus-shared distinction for giving an item back; now always given back to the store | store op: give back (`HostedWork.giveBack`) |
| claude-signed-out.md | 3.5 Files and tests | `app/src/live/claudeSignedOutText.test.ts`, `cardState.test.ts`, and the banner acceptance line | app: Expo app tests |
| compliance-scan/02-decisions.md | 2 Signed-in answers | The Worker test of signed-in answers carrying `no-store` and `nosniff` | hub: cloud Worker tests |
| compliance-scan/03-behavior.md | 3.5.3 The view | The view's `byHand` and `running` fields, built by the agent | store op: `fact_set` carries `byHand`/`running` raw; the store builds the view |
| compliance-scan/03-behavior.md | 3.1 Rules | The rules data (`shared/asvs.ts`: requirements, intent, remediation, assessor and not-applicable reasons) and the requirement roll-up with `worstStatus`; the rest of Level 1 | store op: `scanProbesSummary` and the shared ASVS rules |
| compliance-scan/03-behavior.md | 3.2 The probes | Pointer to the rules and Needs-an-assessor/Not-applicable marking; first-run history on retaskable.com | store op: shared rules and roll-up |
| compliance-scan/03-behavior.md | 3.2 The chore | Building `ScanProbesView` (`probesView`), the day's "already run" check, the hour | store op: `fact_set` on `scan_probes_raw` and `scan_probes` summary; `jobs_due` |
| compliance-scan/03-behavior.md | 3.2 The Worker's side | `no-store` on every `/api/*` answer and its threat-model test | hub: cloud Worker (`cloud/src/index.ts`, `cloud/test/threatModel.test.ts`) |
| compliance-scan/03-behavior.md | 3.3 Its time; Stays manual; counted view | Counting the sweep's routes and rules into a finished view; the Accessibility tab's manual list | store op: `scanA11ySummary` on `scan_a11y_raw`; app: Accessibility tab |
| compliance-scan/03-behavior.md | 3.4 The view and the public page | The `ScanView` shape, the public projection, the Compliance tab (70 Level 1 requirements, counts, Beyond Level 1, Extra hardening) and the Accessibility tab's criteria counting | store op: `scanProbesSummary`/`scanA11ySummary`, shared/public projection; app: Compliance and Accessibility tabs |
| compliance-scan/03-behavior.md | 3.5 Run now | The **Run now** button, **Running…** state, refusal text under the line, "by hand" wording | app: Expo app Chores screen |
| compliance-scan/03-behavior.md | 3.5.1 The command | `chore_run` schema, owner-only role, not routed to other computers, phone app allowed, `/demo` answer, `POST /api/chores/run` | hub: cloud commands (`chore_run`), project roles |
| compliance-scan/03-behavior.md | 3.6.1, 3.6.9 | The `checks` entries in the finished view, the Accessibility tab's counts, the Chores-screen log line | store op: `scanA11ySummary`; app: Accessibility tab |
| compliance-scan/03-behavior.md | 3.2 Chores screen and page's view | The **Security probes** panel, `probesHint`, `probesRunLine`, `probesTeamPatch`, `State.scan`, `scanProbesView` in the snapshot | app: Expo app Chores screen; hub: settings commands |
| compliance-scan/04-acceptance.md | scan-public; chore-run-now | The public-projection test, the Worker refusal of a member's command, the phone app sending it | hub: cloud Worker; app |
| daily-lighthouse.md | 2 One run a day | Local and file-backlog projects "just run" without a lock (no-store behaviour) | store op: `chore_lock` (every project has a store; fails closed) |
| daily-lighthouse.md | 4 Acceptance | Agents-screen line showing the day's scores | app: Agents screen |
| daily-lighthouse.md | "Filled in from the saved reports" | Rebuilding a view with no `days` from the saved reports | store op: the store rebuilds from its own stored reports |
| daily-lighthouse.md | 3 view (`withDay`, `a11yOf`, `securityChecks`, `days`, `security`, `a11y`) | Building the 14-day trend and the first page's `a11y` and `security` views, and the snapshot copy for the public page | store op: `lighthouseSummary` in the store (`fact_set` on `lighthouse_raw`) |
| daily-lighthouse.md | 3 State and Agents line | `State.lighthouse` and the "Lighthouse at 06:12" line | store op: the finished `lighthouse` summary from `fact_set` on `lighthouse_raw`; app: Agents screen |
| daily-lighthouse.md | 2 Config; 3 Chores screen | `lighthouse` switch, pages field, Save, `settings_request`/`settings_set` flow, `lighthouseHint`/`lighthouseText`, `siteSettings`, validation | app: Expo app Chores screen; hub: settings commands |
| daily-lighthouse.md | 2 One draft per day | Counting open chore drafts on this computer (`openChoreDrafts`) | store op: open chore drafts count (as `chore_next` counts them) |
| daily-lighthouse.md | 2 New only; 3 | Local copy of known finding keys and draft statuses kept in the project state file | store op: known-findings keys with draft ids (`knownStore`), draft status |
| daily-lighthouse.md | 2 When; 3 scheduler | The agent's own once-a-day schedule (hour, time zone, paused, "result not from today") | store op: `jobs_due` |
| engines/03-choosing-the-engine.md | 3.4 Cursor model names (the chat) | A request's plan progress line in the chat ("Writing the plan", "Waiting its turn: 2 plans ahead", `draftProgressLine`) | app: Expo app Ask screen |
| engines/03-choosing-the-engine.md | 3.4 Cursor model names (the chat) | `summaryItems`: the agent building a shortened local state of open and latest 10 finished items for Ask | store op: read of open items and latest finished (the agent asks the store, keeps no local item list) |
| engines/03-choosing-the-engine.md | 3.4 Cursor model names (What you see, Agents tab, demo) | Setup/React-site boxes for Cursor models, Agents tab chips, the demo's mock settings | app: Expo app Settings and Project settings; demo is the site's |
| engines/03-choosing-the-engine.md | 3.1 Per computer and per role | Where the engine choice is shown on the site (Settings → Agents `computerRows`, Project settings' "Which engine" panel) | app: Expo app computer rows and Project settings |
| engines/03-choosing-the-engine.md | 3.2 Models and the usage limit | The "Waiting for limit" status text and its "Cursor roles carry on" tooltip | app: Expo app status chip, from the computer's reported `cursorCarriesOn` |
| engines/03-choosing-the-engine.md | 3.2 Models and the usage limit | Choosing which agent takes a given-back item next (queue order) | store op: claim/queue order (`claim_next`); agent only gives the item back |
| engines/06-permissions-the-allowlist.md | 6.4 Extra allow entries | The "Cursor may also run" box, its hint text and red error display; the `GET /api/setup` payload and "Where you see it" notes | app: Expo app Engines settings (agent keeps the checks and the file written) |
| engines/07-unknown-cost.md | 7 Unknown cost | Today's-spend line ("$4.20 + 3 Cursor sessions"), the Agents tab "cost not reported" and "Cursor: usage not reported" lines, the Specs numbers, retaskable.com and demo display | app: Expo app header, Agents tab and numbers |
| engines/07-unknown-cost.md | 7 Unknown cost | `ItemStats` sums of known costs and `unknownCostSessions` count | store op: item stats / cost sum (store computes from per-session `costUsd`) |
| engines/09-setup.md | 9 Setup | Setup's engine field and phone Settings, the retaskable.com `SiteEngines` panel and its wording, the app's Settings → Engines and what it lacks | app: Expo app Settings → Engines and Project settings |
| first-run-check.md | 2. Decisions: the `/start` page, the home redirect, hiding Setup in the profile menu | Where the page opens and when the profile menu's Setup row hides | app: Expo app, `/start` and the profile menu |
| first-run-check.md | 3.4 Setup leaves the profile menu | `setupCompleteFor`, `setupComplete` and `phoneSetupComplete` deciding when the profile menu's Setup row hides | app: Expo app, profile menu |
| first-run-check.md | 3.2 The page | The five (now nine) step cards, badges, trial-change button, Check again, I'm done here, auto-refresh and the home's redirect (`app/src/start.tsx`, `startText.ts`) | app: Expo app, `/start` screen |
| first-run-check.md | 3.3 The trial plan | "One trial at a time per project" rule in `createDraft` | store op: draft creation (the store refuses a second open trial draft) |
| first-run-check.md | 4. Tests and 5. Acceptance | `shared/cloud.test.ts`, `app/src/startText.test.ts`, `app/src/cloud/commands.test.ts`, and the empty-account-to-trial-plan and profile-menu acceptance lines | app: Expo app tests (the command tests are cloud commands) |
| installers/03-the-bundle-agent-bundle.md | 3.11 Details (agent-self-update): the feed | `GET /api/agent/release` route, its R2 bucket, `<os>-<arch>.json` entries, validation and caching (`cloud/src/agentRelease.ts`) | hub: release feed route, serves the entry; the agent only reads it |
| installers/03-the-bundle-agent-bundle.md | 3.16 Details (staging-builds): served, uploaded, CI publish | `GET /api/agent/download/<version>/<file>` route, `npm run upload-release` writing R2 objects and feed entries, Woodpecker publish step and secrets | hub: package download route and release bucket (the CI upload is the build's, not the agent's) |
| installers/03-the-bundle-agent-bundle.md | 3.18 Details (linux-release): Getting started | Production feed making the Linux download button show, and its libfuse2 / AppIndicator words | app: Expo app, `/start` download step |
| installers/03-the-bundle-agent-bundle.md | 3.17 Details (staging-builds): the download | Getting started's Link this computer step reading the feed and offering Download for Linux (`app/src/start.tsx`, `startText.ts`) | app: Expo app, `/start` download step |
| installers/04-every-agent-on-the-shared-backlog-shared.md | 4. Every agent on the shared backlog | The website's This computer settings showing the store's link state | app: Expo app, This computer settings |
| installers/06-macos-and-linux.md | 6.4.10 Getting the download | `/start` first step on a Mac: Apple silicon and Intel buttons with sizes, the "Not sure?" chip line, the three steps, Staging's variant, and the sketch | app: Expo app, `/start` install step |
| installers/06-macos-and-linux.md | 6.4.1 slice 3 and 6.4.11 Tests | Getting started's download as part of the Production slice; `app/src/startText.test.ts` | app: Expo app, `/start` tests |
| installers/09-tests.md | 9. Tests (self-update, staging-builds, linux-release) | Cloud and app test files: `cloud/test/agentRelease.test.ts`, `app/src/live/restartText.test.ts`, `app/src/startText.test.ts` | hub: release feed and download route tests (agentRelease); app: restart words and download words |
| local-state-diet.md | 1, 2, 3, 4 | Capping item and draft timelines at 200 entries, dropping merged drafts' file contents after 7 days, and their tests: local copies of item and draft records | store op: items and drafts live only in the store (item and draft records, timeline notes) |
| play-on-phone.md | 1, 5 | `POST /api/phone/play`, `/uninstall-and-play`, `/stop` on the agent's own port (a command on the port) | hub: `phone_play` and `phone_stop` cloud commands only |
| play-on-phone.md | 2 | `GET /api/setup?repoPath=` as the way Setup suggests the `phone` section, and the greyed-out hint wording in the UI | app: Expo app Setup (the agent still sends the derived suggestion) |
| play-on-phone.md | 3, 5 | The button, its labels, greyed-out reasons, Stop button, failure and "Opened on" chips | app: Expo app Run controls |
| play-on-phone.md | 4 | The dialogs (install warning, signature dialog) as UI | app: Expo app; the agent keeps the warning text check |
| play-on-phone.md | 6 | The site's waiting lines ("Waiting for someone at <machine>"), `/demo` and Sample rules | app: Expo app; hub: demo |
| tray.md and tray/02-decisions.md | 2 Decisions 1 and 5 (local environment, "everything else is on retaskable.com") | Running retaskable.com locally with its own D1, sign-in and the environment strip | hub: cloud environments (cloud.md section 9, environments.md) |
| tray/03-one-website-three-environments.md | 3 One website, three environments | The ENVIRONMENT sweep test, per-environment D1/R2/DO/secrets, `smoke:local`, "run local whole" | hub: cloud environments (cloud.md section 9); the agent keeps only "one token per environment, nothing crosses" |
| tray/04-the-tray-agent.md | 4.2 The menu, Open on retaskable.com | Which project the site opens on (last-opened project) | app: Expo app routing |
| tray/04-the-tray-agent.md | 4.2 and 4.6 notifications | Requests (N) list, the notification per request, Allow and Deny buttons on the Windows toast, `toastAnswers`, `toast-answer.mjs` | hub: cloud commands (requests retired 2026-10-22, cloud.md section 6.4) |
| tray/05-what-retaskable-com-must-do-first.md | 5 What retaskable.com must do first | The gap table and every row's site side: buttons, waiting lines, settings page, Link a project form, Unlink page, Releases tab, `/demo` answers | app: Expo app screens (Settings, Releases, galleries) |
| tray/05-what-retaskable-com-must-do-first.md | 5 rows 4, 9 (settings, remote policy) | Where `settings_request`/`settings` fields were allowed, snapshot `settings` carrying SiteSettings, project fields saved by the site | store op: project settings (project-settings-on-server.md section 3) |
| tray/05.1-asking-the-person-at-the-machine-retired.md | 5.1 Asking the person at the machine (retired) | Whole section: request kind, `pending`, hub deadlines, Allow/Deny at the tray, snapshot `requests`, audit_log `pending`, `/demo` simulation | hub: cloud commands (cloud.md section 6.4); file not copied |
| tray/05.3-private-galleries-and-pictures-from.md | 5.3.2 and 5.3.6 Seeing a private gallery | Browser picture queue, `READ_RATE_LIMIT`, hub 1 MiB refusal and audit rows, `PICTURE_FILE` sites, `/demo` stand-in pictures, gallery screen | hub: cloud rate limits and audit (cloud.md section 7); app: gallery screen |
| tray/05.3-private-galleries-and-pictures-from.md | 5.3.4 and 5.3.7 Pictures in what you send | Browser scaling (1920 px JPEG 0.8), `HeldPictures`, the Add a picture box, hub `picturesRefusal` before parsing, audit `{count, bytes}` | app: Expo app Pictures box; hub: cloud commands |
| tray/05.4-this-computer-s-settings-on-the-site.md | 5.4 This computer's settings on the site | Whole section: `ComputerView` in hello, secrets-by-name view, This computer page, Ask for a change flow, Tags panel, "same machine: not detected" | app: Expo app Settings -> Agents; hub: hello schema; file not copied (superseded by agent-settings.md) |
| tray/06-build-order.md | 6.1.1, 6.1.3, 6.1.4, 6.1.5 app-side | The app's local mode (`agentClient.ts`, `LiveProvider`, Setup files, `Source`, saved "This computer" choice, Getting started wording, demo's Use this computer), the slice plan for the app and the app's acceptance | app: Expo app (expo.md) |
| tray/06-build-order.md | 6.1.2 What the site must do first | The table of local-only routes and their site twins (Link project, engines, team branch, release notes reads, phone stop, gallery draft, item log) | app: Expo app Settings and tabs; hub: cloud commands |
