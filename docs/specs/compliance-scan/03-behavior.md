## 3. Behavior

### 3.1 Statuses

The rules are shared data in `shared/asvs.ts`, beside `shared/wcag.ts`: each ASVS 5.0 requirement with its id, level, title, the probes that give evidence for it (`applies`: `"any"` or `"retaskable"`), its intent and remediation (both go into the draft's prompt), or its assessor or not-applicable reason. The store and the app read them too; the agent reads them to know which probes to run and what to write up. The agent produces one status per probe, from the five below, and the store rolls them up into a requirement's status (the worst of its checks').

| Status | Means |
|---|---|
| `na` | Doesn't apply. Lowest, so it never makes a requirement worse, and never counted as met. |
| `met` | A probe found evidence that satisfies it. |
| `unknown` | No probe could tell (a TLS version Node's OpenSSL can't offer, a page that didn't answer). Shown as not checked, never met or not met. |
| `notMet` | A probe found it violated, or the required header missing. |
| `error` | The probe itself failed. A tool problem, logged, never a finding. |

### 3.2 The probes (`agent/src/securityProbes.ts`)

Plain `fetch` and `node:tls`, against the staging url, at most about 30 requests a run, spaced so retaskable.com's sign-in rate limit ([cloud.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/cloud.md) section 7) never answers `429`. They read the page, a made-up path's `404`, and an API answer where the project has one (`/api/me` signed out, here).

| ASVS 5.0 | Level | Probe | Applies | Was (4.0.3) |
|---|---|---|---|---|
| 12.2.1 | 1 | A page's `http://` address redirects to `https://`. An API path isn't expected to redirect (4.1.2, Level 2, asks that it doesn't) | any | 9.1.1 |
| 12.1.1 | 1 | TLS 1.2 or 1.3 accepted, 1.0 and 1.1 refused (`unknown` where Node can't offer them) | any | 9.1.3 |
| 12.2.2 | 1 | The certificate is publicly trusted, and valid for more than 14 days | any | new |
| 3.4.1 | 1 | `Strict-Transport-Security` on every answer, `max-age` of at least a year (`includeSubDomains` is Level 2) | any | 14.4.5 |
| 4.1.1 | 1 | Every answer with a body has a `Content-Type` with a charset | any | 14.4.1 |
| 3.2.1 | 1 | API answers can't be rendered as a page: `Content-Disposition: attachment`, a CSP `sandbox`, or a `Sec-Fetch-*` check | retaskable | 14.4.2 |
| 3.4.2 | 1 | An `Origin: https://scan.invalid` request isn't echoed in `Access-Control-Allow-Origin`; `*` only on answers with nothing private | any | 14.5.3 |
| 3.3.1 | 1 | Every cookie is `Secure`, and named `__Host-` or `__Secure-` | any | 3.4.1 |
| 3.4.3 | 2 | A CSP with `object-src 'none'`, `base-uri 'none'` and scripts by nonce or hash | any | 14.4.3 |
| 3.4.4 | 2 | `X-Content-Type-Options: nosniff` on every answer | any | 14.4.4 |
| 3.4.5 | 2 | A `Referrer-Policy` that keeps the path from other sites | any | 14.4.6 |
| 3.4.6 | 2 | `frame-ancestors` in the CSP on every answer (`X-Frame-Options` alone doesn't count) | any | 14.4.7 |
| 3.3.2, 3.3.3, 3.3.4 | 2 | Cookies' `SameSite`, `__Host-` and `HttpOnly` | any | 3.4.3, 3.4.4, 3.4.2 |
| 16.5.1 | 2 | The `404` and the API's error answer hold no stack trace, file path or `at Object.` | any | 7.4.1 |
| 14.3.2 | 2 | `Cache-Control: no-store` on signed-in API answers (a test token, section 2) | any with a token | 8.2.1 |
| 3.4.8 | 3 | `Cross-Origin-Opener-Policy` on pages | any | new |
| 3.6.1 | 3 | Scripts and stylesheets from other sites carry `integrity` (read from the sweep's pages) | any | 14.2.3 |
| 13.4.6 | 3 | No version in `Server` or `X-Powered-By` | any | 14.3.3 |

**Outside ASVS**, kept as **Extra hardening** as the tab shows now: Trusted Types (`require-trusted-types-for`), `Permissions-Policy`, `/.well-known/security.txt` (RFC 9116, `Expires` in the future; ASVS's Appendix D recommends it but no requirement asks for it), and no location or notification prompt on load. ASVS 5.0 names none of them.

The other Level 1 requirements are marked **Needs an assessor** or **Not applicable** by the store from the shared rules; no probe runs for them. The probes are evidence for the requirements they name, not an audit.

- **The probes** (`agent/src/securityProbes.ts`, `runProbes(target, deps)`): up to 5 of the pages, the made-up path `/retaskable-scan-not-found`, on retaskable.com `/api/me` sent with `Origin: https://scan.invalid` (elsewhere the first page, for CORS alone), its `OPTIONS` preflight, `/api/me` again as a page would ask for it (`Sec-Fetch-Dest: document`), the first page over `http://`, and `/.well-known/security.txt`: 11 requests on this repo, 250 ms apart, none followed through a redirect, then a TLS handshake as Node offers it and one each allowing only TLS 1.0 and 1.1 (at OpenSSL's security level 0). "Every answer" is each of those GETs that isn't a redirect; "pages" are the pages' answers. A probe with nothing to read is `unknown` ("No page of the staging site answered"), one that throws is `error`, and each result carries a detail for the write-up only. `retaskable` is a staging host on `retaskable.com`; elsewhere 3.2.1 is `unknown`. 3.2.1 is met by `Content-Disposition: attachment`, a CSP `sandbox`, or the page-style request refused with 400, 403 or 406. 4.1.1 asks a charset of `text/*` and XML types only (JSON defines none). 3.6.1 reads the pages' own HTML (`<script src>` and stylesheet `<link>` on another host, `foreignWithoutIntegrity`), not yet the sweep's. Cookie probes with no cookie set signed out are `unknown`. 14.3.2: see the token below. The real `fetch` and `tls` deps refuse any method but `GET`, `HEAD` and `OPTIONS`.
- **The chore** (`agent/src/scanChore.ts`, beside `lighthouseChore.ts`, called from the orchestrator's `step` after it): `probesTarget(cfg)` (null while `scan.probes` is off; `{ note }` without a staging url, or when the staging url's origin is production's; pages from `scan.pages`, else `lighthouse.pages`, else `/`), due when the store's `jobs_due` names `scan` (the store decides the day and the hour), a lane free; then `ChoreLocks.take("probes-<day>")` (`{ by }`: "<login>'s agent runs the probes today"), dropping the day before's. `probeFindings`: each requirement or hardening check `notMet`, keyed `probes <id>` (`probes 3.4.5`, `probes security-txt`), new by `isNewFinding` (lighthouse.ts). Under the limit of 2 open chore drafts (the store's count, which includes `[Scan]`), `scanRequest` renders `prompts/scan.md` (every status, each new finding with what its probes saw, its intent and remediation, and the keys) after `SCAN_TAG` `[Scan]`, the drafter runs it on the chore tier, and each key is recorded with the draft; past the limit nothing is recorded and the note says so. A probe's `error` is in the note, never a finding. What goes up is statuses and counts only (section 3.4), never a detail or a path. A hardening check keeps its own status (`unknown` and `error` included), same as a requirement's, so a real failure can be told from one it couldn't check.
- **The requirement and hardening statuses are the store's to roll up** (2026-10-25, item `summaries-move-to-store`, [last-local-state.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/last-local-state.md) section 3.4): the chore calls `fact_set` on `scan_probes_raw` with that run's per-check results, `ms`, `newFindings`, `drafted?`, `note?` and each finding key's `fixQueued` (decided here, from this computer's own knowledge of open `[Scan]` drafts). The store rolls the checks up (`shared/factSummaries.ts`'s `scanProbesSummary`) and answers the finished view. The agent keeps no `scan.probes.view` and no `worstStatus` roll-up.
- **Config**: `scan: { probes, pages, signedInPaths }` (`ScanConfig`), a project setting kept in the store (`signedInPaths` up to 20 paths). The agent reads it. `pages` and `signedInPaths` are set by hand for now (a path without its `/` gets one).
- **The staging test token** (slice 4, `agent/src/scanToken.ts`): a signed-in session on the staging site, in this machine's credential store (service `retaskable-scan-token`, account the staging origin), set with `npm run scan-token` (`retaskable scan-token`, `agent/src/cli.ts`), which asks the staging address (https only) and the token, unseen. On retaskable.com it's the value of a test account's `__Host-retaskable_session` cookie, which `getSession` takes as `Authorization: Bearer` too; it lasts as long as that session. The chore reads it only at the run (`realDeps`'s `probe`, never `probesTarget`, so no view is made from it), for the staging origin alone. With it and `scan.signedInPaths`, the probes send a `GET` to each of up to 5 of those paths with `Authorization: Bearer <token>` after the others (16 requests at most here), and 14.3.2 is met when every `2xx` answer's `Cache-Control` holds `no-store`, not met naming the paths without it. No token is `unknown` ("needs a staging test token (npm run scan-token)"), as are no paths ("needs signed-in paths (scan.signedInPaths)") and no `2xx` answer ("No signed-in path answered with the test token (/api/me 401)", a token refused or expired). The token is in no detail, draft or view. This computer's settings page names it, by name only, "Staging test token for https://staging.retaskable.com · Set" (`scanTokenName`; `computerView`'s `scanToken`, asked whether it's there at most once a minute per origin), and the token's value goes nowhere. The signed-in answers count for 14.3.2 alone: the cookies they set aren't read yet.
- **The screens and the Worker**: the Chores screen's **Security probes** panel and its lines, the page's view (`State.scan`) and the Worker's `no-store` header and its threat-model test are the app's and the cloud's, not the agent's. The switch is a project setting the owner saves through the hub ([cloud.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/cloud.md) section 6.1); the agent asks the store for it.
- **A person's to do on this repo**: `npm run scan-token` on the computer that runs it, with a test account's session on staging; `scan.signedInPaths` (`["/api/me", "/api/projects"]`) in its config; then turn on the daily security probes on the Chores screen.
- **Tests**: `agent/src/securityProbes.test.ts` (every row met on a clean fake site, each not met, unknown and error, only `GET`/`OPTIONS` to the staging host, today's retaskable.com finding exactly 3.2.1, 3.4.5, Permissions-Policy and security.txt; with a token, only the signed-in paths' GETs carry it and no detail holds it, 14.3.2 not met without `no-store`, and unknown with no token, no paths or a refused token), `agent/src/computerView.test.ts` (the token by name for this project's staging origin only) and `agent/src/scanChore.test.ts` (the target and production refused, the signed-in paths and never the token, one `[Scan]` draft then nothing new the next day, paused, no lane, another agent's lock, the limit, a probe's error).

### 3.3 The accessibility sweep (`agent/src/a11yScan.ts`)

- **What it loads**: each of `scan.pages`, signed out, and on retaskable.com every screen on the Sample (`/<screen>?sample`, the list `npm run shoot:ui` photographs; it moves from `app/scripts/shoot-ui.mts` to `app/scripts/sampleRoutes.ts` so both read it). A user's project gets signed-in screens only if its app has a similar mode.
- **What runs where**, to keep it near a minute:
  - Phone size (390×844), light: all of axe-core's rules tagged `wcag2a`, `wcag2aa`, `wcag21a`, `wcag21aa` and `wcag22aa`.
  - Phone size, dark (`emulateMedia({ colorScheme: "dark" })`, and on retaskable.com the `scheme` pref in `retaskable.expo.ui`): `color-contrast` only, the rule a theme changes.
  - Desktop size (1440×900), light and dark: `color-contrast` and `target-size`; light only: the keyboard checks below (section 3.6.1 gives each of them its size and theme).
  - Axe's best-practice rules are reported apart, tagged as the Accessibility tab already tags them (`shared/wcag.ts`).
- **What axe can't check** (item scan-a11y-keyboard, written out in section 3.6), plain code in the same page:
  - **Keyboard** (2.1.1, 2.1.2): Tab up to 60 times, recording the focus order. Focus that stops moving, or cycles among a few elements, is a trap; an interactive element focus never reached is a finding.
  - **Focus visible** (2.4.7): each focused element's outline or shadow differs from its unfocused style.
  - **Focus not hidden** (2.4.11): the point under a focused element's centre is that element, not a sticky bar.
  - **Reflow** (1.4.10): at 320 px wide, nothing scrolls sideways.
  - **Text spacing** (1.4.12): with WCAG's spacing CSS added, no text is clipped.
  - **Page title** (2.4.2): each route has its own title.
- **Keys**: `a11y <route> <size> <theme> <rule id>`. Never a selector: react-native-web's class names change between builds. A rule that fails on every route is one finding listing its routes.
- **Its time** goes up with the run's result (`ms`). Over two minutes on this repo is a finding of its own: the sweep needs trimming.
- **Locally**: `npm run scan:a11y` (`app/scripts/scan-a11y.mts`) runs the same module against the local build, as `npm run shoot:ui` does, so a change can be checked before it merges.
- **Stays manual**, as "Needs a person" (the store and app say so): screen-reader passes, whether labels and alt text make sense, error messages (3.3.x), meaning carried by colour alone, and the Expo Go app on phones, which a web scan doesn't touch.
- **The counted view is the store's to build** (2026-10-25, item `summaries-move-to-store`, [last-local-state.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/last-local-state.md) section 3.4): `a11yScan.ts` no longer counts its own routes, rule results and the checks axe can't make into a finished view. It calls `fact_set` on `scan_a11y_raw` with that run's routes, rule results and checks, plus `newFindings`, `drafted?` and `note?`; the store counts them (`shared/factSummaries.ts`'s `scanA11ySummary`) and answers the finished view, which `npm run scan:a11y` uses the same way it used the view the sweep used to build itself.

### 3.4 What goes up

- **Raw results only.** The probes' last day and known findings, and the accessibility baseline (`baseline`), are the backlog store's `scan_probes` and `scan_a11y` results ([last-local-state.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/last-local-state.md) section 3.4), so every computer and the site read the same ones. The agent sends each run as a `fact_set` on `scan_probes_raw` or `scan_a11y_raw` and the store builds the views.
- Results, counts and the run's time only: never a route path (a user's paths may be private) and never a probe's raw answer.
- The projection to the public page, the Compliance tab (OWASP ASVS 5.0 Level 1: its 70 requirements, counts, Beyond Level 1, Extra hardening) and the Accessibility tab's criteria counting are the store's and the app's; the agent asks nothing of them. A WCAG criterion counts as checked automatically only when an axe rule or a check in section 3.3 covers it, from every route, size and theme the sweep loaded.

### 3.5 Run now (item chore-run-now)

- The app shows a **Run now** button beside each of Lighthouse, the probes and the sweep, when that chore is on and an agent that runs the project is online.
- It sends one fixed command, `chore_run { project, kind }` with `kind` one of `"lighthouse"`, `"probes"` or `"a11y"`, on [cloud.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/cloud.md) section 6's list. It runs only that chore's own fixed code against the project's staging url: no argument names a url, a path or a command. Who may send it (an owner of the project) is the hub's check. The agent checks it itself: the chore is on, the project has a staging url, the agent isn't paused, the same kind isn't already running, and that kind hasn't been run by hand in the last 10 minutes. A refusal says which ("Lighthouse is already running").
- A run by hand is the day's run as far as findings go (new only, one draft, the limit of 2), and takes the same lock with the hour added (`lighthouse-<day>-<hh>`), so two agents on a shared backlog don't both run it.

The rest of this section fills in the agent's side from the code as it is on 2026-10-19 (`agent/src/lighthouseChore.ts`, `agent/src/scanChore.ts`). Every line below is a **[Proposal]**, the human's call; where one reads the lines above more narrowly than they could be read, it says so.

#### 3.5.1 The command

- **[Proposal]** The command's schema, role, routing, demo answer and the page's path are the hub's and the app's ([cloud.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/cloud.md) section 6, [project-roles.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/project-roles.md) section 3.1): `chore_run: { kind: "lighthouse" | "probes" | "a11y" }`, strict, owner only, to the hub's own agent only. The agent receives it only through the cloud link.
- **[Proposal]** **It answers at once.** The agent's `ack` is `{ started: true }`, sent as soon as its checks pass, never after the run: Lighthouse alone takes about a minute a page (two presets), well past the hub's wait for an answer. What the run found comes up as a scheduled run's does.
- Logged as every command is, on both ends ([cloud.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/cloud.md) section 6.3), with its `kind`.

#### 3.5.2 What the agent checks, in order

- **[Proposal]** After [cloud.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/cloud.md) section 6.2's checks, the first that fails is the `nack`, in these words (`choreRunRefusal(o, kind, now)` in `agent/src/choreRun.ts`, pure, beside the three chores):

| Check | Refusal |
|---|---|
| The chore is on (`lighthouse.enabled`, `scan.probes`, `scan.a11y`) | "Lighthouse isn't on for this project." (or "The security probes …", "The accessibility sweep …") |
| Its target has no note: a staging url, and for the probes and the sweep one that isn't production's (`lighthouseTarget`, `probesTarget`, and the sweep's own) | The target's note, as the Chores screen shows it |
| The project isn't paused | "The project is paused." |
| Not over the usage limit (`canPlan()`): its write-up runs the drafter | "Over the usage limit: it can run once it resets." |
| No run of that kind under way, scheduled or by hand (each chore's `running` set) | "Lighthouse is already running." |
| None started by hand in the last 10 minutes on this agent (`RUN_NOW_GAP_MS`, kept in memory: a restart forgets it) | "Lighthouse ran by hand at 14:02. It can run again from 14:12." |

- **[Proposal]** The first four are the scheduled run's own conditions (the store's `jobs_due`, the targets); the last two are only a hand run's. The lines above don't name the usage limit; it's added here so a press can't spend past it.
- **[Proposal]** **Not a free lane.** A scheduled run waits for one (`laneFree`) so it doesn't slow the day's sessions; a hand run doesn't, since a person asked for it now. Lighthouse's performance score may read lower while sessions build; the run's line doesn't say so.
- **[Proposal]** **No Chrome.** A run by hand tries again even after Chrome was found missing (`noChrome`, which otherwise waits for a restart): installing Chrome and pressing **Run now** is how a person finds out it worked. Still missing, the run ends with the chore's own note ("Lighthouse needs Chrome on this computer"), not a refusal, since the agent only learns it by trying.

#### 3.5.3 The run

- **[Proposal]** It runs the same function as the scheduled run (`runDay` for Lighthouse, the probes' and the sweep's own), told it's by hand, outside any session and holding no lane, as section 2 says of the chores.
- **[Proposal]** **The day stays the scheduled run's.** A hand run doesn't set `lighthouse.day` (or `scan.probes.day`, `scan.a11y.day`), so a press at 07:00 doesn't stand in for the day's run after `lighthouse.hour`, and a press after it doesn't stop tomorrow's. This reads "the day's run as far as findings go" as being about findings only: a hand run and the scheduled one each write up only what's new (the keys in `known`, `isNewFinding`), so pressing twice in a day, or once before the scheduled run, never writes the same finding up twice.
- **[Proposal]** **The lock.** It takes the store's lock `<kind>-<day>-<hh>` (`lighthouse-2026-10-19-14`, `probes-…`, `a11y-…`; `hh` the agent's local hour, two digits), dropped when the run ends rather than kept for the day. Held by another agent, the run doesn't start and, the `ack` having gone, the view's note says it: "@andrew's agent ran Lighthouse by hand this hour."- **[Proposal]** **What goes up** is the chore's one raw result, replacing the day's as a scheduled run's does (the store keeps one entry a day, so a later run that day replaces that day's scores), and its reports under `lighthouse/<day>/` are written over. The raw row carries `byHand: true` and, while a run of that kind is under way (by hand or not), `running: true`, set when it starts and dropped when the result is written, so everyone looking sees it, after a reload too. A run that fails ends with its note, as now ("Lighthouse failed: …"), and `running` dropped. Neither field reaches the public page.
- **[Proposal]** **At the limit of 2 open chore drafts**, a hand run's note says "these wait for the next run" instead of "tomorrow's run", since a person may press again once a draft is approved or discarded.

#### 3.5.4 What the person sees

- **[Proposal]** The button, its **Running…** state, a refusal's reason and the "by hand" wording of the last-run line are the Expo app's. In the 10 minutes after a hand run a press is refused with the time it can run again: the agent's clock decides, not the page's.

### 3.6 The checks axe can't make, in detail (item scan-a11y-keyboard)

Section 3.3's six checks, written out so they can be built without guessing. **[Proposal]**, every line of this section. They run inside the sweep (`a11yScan.ts`), on its routes, after axe on each route, and add no chore, lock, config switch or command of their own: with `scan.a11y` off they don't run either.

#### 3.6.1 The code's shape

- **`agent/src/a11yChecks.ts`**, in two halves. *Collectors* are functions passed to Playwright's `page.evaluate`: they read the page and return plain data (numbers, roles, names, booleans), never a node or a selector. *Judges* are pure functions over that data (`judgeWalk`, `judgeFocusStyles`, `judgeObscured`, `judgeReflow`, `judgeSpacing`, `judgeTitles`) and return each check's pass or failures. The judges are tested in node with no browser, the collectors against fixture pages (section 3.6.9).
- **Check ids** fill the `<rule id>` slot of section 3.3's key, and each is mapped to its criterion in `shared/wcag.ts`'s `AUDIT_CRITERION` beside axe's rules, so the store counts them as it counts audits (`wcagResults`). The `scan-` prefix keeps them apart from axe's rule ids, none of which starts with it.

  | Check id | Criterion | Size and theme |
  |---|---|---|
  | `scan-keyboard-unreached` | 2.1.1 Keyboard (A) | desktop, light |
  | `scan-keyboard-trap` | 2.1.2 No Keyboard Trap (A) | desktop, light |
  | `scan-focus-visible` | 2.4.7 Focus Visible (AA) | desktop, light |
  | `scan-focus-not-hidden` | 2.4.11 Focus Not Obscured (Minimum) (AA) | desktop, light |
  | `scan-reflow` | 1.4.10 Reflow (AA) | 320×256, light |
  | `scan-text-spacing` | 1.4.12 Text Spacing (AA) | phone (390×844), light |
  | `scan-page-title` | 2.4.2 Page Titled (A) | desktop, light |

- **Light only**: a theme changes colours, not what takes focus, where things sit or what a page is called. Retaskable's focus ring is drawn from the theme's own tokens (`focusRing`, `app/src/ui/pointer.ts`), so one that's there in light is there in dark; whether it stands out enough is 2.4.13 (AAA), outside the target.
- **One walk serves four checks**: unreached, trap, focus visible and focus not hidden all read the same Tab walk (section 3.6.2), so each route is walked once.
- **In what goes up**, each check id is one `checks` entry in the raw result, `pass` only when every route that ran it passed. A check that ran on no route (each load failed, or no walk reached anything) is left out, so the criterion shows as not checked automatically rather than passed.

#### 3.6.2 The Tab walk

- **The start**: the route loaded as section 3.3 loads it (on the Sample, as `shoot-ui.mts`'s `open`: network idle, then the Sample's clock run 3 seconds), then focus put back on the document (`document.activeElement.blur()`).
- **Each press**: `page.keyboard.press("Tab")`, then time for the page to draw its focus state. The Sample's clock is Playwright's fake one, which holds animation frames too, so on the Sample the clock is run 50 ms (`page.clock.runFor(50)`) instead of waiting for frames; signed out, with the real clock, two animation frames are waited for. Then the collector records the focused element.
- **What's recorded**: a number the collector gives each element the first time it's focused (kept in a `WeakMap` in the page, so the same element always gets the same number and no two are confused), its role (`role`, else its tag), its accessible name (`aria-label`, else its text, trimmed to 60 characters), its box (`getBoundingClientRect`, after the browser has scrolled it into view), and its focused styles (section 3.6.3). Focus on `body`, or none, is recorded as "the page".
- **The end**, whichever comes first: focus back on the first element focused (a full round, which is how a page should behave); focus on the page after at least one element (it left for the browser's own controls, which is a full round too); or 60 presses (cut short, not a finding on its own: a Board with many cards has more than 60 stops).
- **Its cost**: about 30 ms a press, so under 2 seconds a route at 60 presses.

#### 3.6.3 Focus visible (2.4.7)

- **For each element the walk reached**, the collector reads its computed `outline-style`, `outline-width`, `outline-color`, `box-shadow`, `border-color` and `background-color` while focused, and again once it has lost focus to the next press (read in the next press's step, so the check adds no presses; the last element's unfocused styles are read after a final `blur()`).
- **It passes** when any of them differ. Retaskable's own components draw `focusRing`'s `box-shadow` (`buttons.tsx`, `menus.tsx`, `panels.tsx`) or change `borderColor` (inputs, `primitives.tsx`), and both count. A background change alone counts too: 2.4.7 asks for a visible indicator, not a particular one.
- **It fails** when nothing differs: typically a `Pressable` built outside `app/src/ui` with `outlineWidth: 0` and no ring of its own.
- **Not judged**: an element with a 0×0 box (focusable but not drawn; section 3.6.4 reports it).

#### 3.6.4 Keyboard: traps, and what Tab never reaches (2.1.1, 2.1.2)

- **A trap** is focus that stops moving, or goes round a few elements without returning to the first one or to the page: the same element on two presses in a row, or the last 10 presses visiting 3 or fewer elements, none of them the first. Before that's a finding the walk presses **Escape** once and Tab again; if focus then leaves the circle, it isn't a trap. This is for modals: react-native-web's `Modal` holds focus inside itself on purpose (`bellList.tsx`'s full-screen list says so), and Escape closing it is a way out WCAG accepts.
- **Unreached**: the collector lists the page's *candidates*, what a keyboard user must be able to reach: `a[href]`, `button`, `input`, `select`, `textarea`, `summary`, anything whose `role` is `button`, `link`, `checkbox`, `switch`, `radio`, `tab`, `menuitem`, `option` or `combobox`, and anything with a `tabindex` of 0 or more. Left out: anything disabled (`disabled`, `aria-disabled="true"`), hidden (`display: none`, `visibility: hidden`, a 0×0 box, `inert`, or inside `aria-hidden="true"`), and anything behind an open modal. A candidate the walk never focused fails, named by role and name: mostly a `Pressable` given `focusable={false}`, or a view with a role and a click handler but no tab stop.
- **A walk cut short at 60** judges only the candidates before the last element it reached, in document order, so a long Board isn't reported for the cards after the 60th.
- **Not walked**: Shift+Tab. A page reachable forwards is very rarely unreachable backwards, and it would double the walk.
- **Not claimed**: whether the order makes sense (2.4.3 Focus Order) needs a person, and stays "Needs a person" on the tab.

#### 3.6.5 Focus not hidden (2.4.11)

- **For each element the walk reached**, after the browser has scrolled it into view, the collector asks `document.elementFromPoint` at five points of its box: the centre, and 2 px in from each corner, each kept inside the viewport.
- **It fails** only when **none** of the five lands on the element or something inside it, since 2.4.11 at AA asks only that focus isn't *entirely* hidden. An element half under a bar passes here (that's 2.4.12, AAA). This replaces section 3.3's first draft, "the point under a focused element's centre".
- **What it's for**: `Pinned` (`app/src/ui/layout.tsx`) is sticky at the top or foot of the scrolling page on the web, so a header or the chat box can sit over the element the browser just scrolled to. The finding names the element and what covers it, by role and name ("button 'Retry', under region 'Chat'").
- **Not judged**: an element still outside the viewport after scrolling, and one with a 0×0 box.

#### 3.6.6 Reflow (1.4.10)

- **Each route** is loaded again in a 320×256 viewport (WCAG's measure for 1280 px at 400% zoom), in a page of its own so the walk's state doesn't carry over.
- **It fails** when the page itself scrolls sideways: `document.documentElement.scrollWidth` more than its `clientWidth` plus 1 (a pixel of rounding). The finding gives the width it needed ("needs 412 px at 320").
- **Allowed**: a scroller of its own inside the page (`overflow-x: auto` or `scroll` on an element that isn't the document) scrolling sideways, WCAG's exception for content that needs two dimensions (the Board's columns, a code block, a table). Only the document is judged.
- **Not judged**: the height. A page taller than 256 px scrolls down, as WCAG allows.

#### 3.6.7 Text spacing (1.4.12)

- **At phone size**, where text has least room, the collector notes every element whose own text overflows a box that hides it (`scrollWidth` or `scrollHeight` more than its client size plus 1, with `overflow` computed as `hidden` or `clip`). Then it adds WCAG's spacing as a style sheet, `* { line-height: 1.5 !important; letter-spacing: 0.12em !important; word-spacing: 0.16em !important }` and `p, [role="paragraph"] { margin-bottom: 2em !important }`, lets the page draw (the clock's 50 ms, or two frames) and looks again.
- **It fails** for an element that overflows now and didn't before: the spacing pushed its text out of a box that hides it.
- **Not a finding**: an element that already overflowed before (comparing the two is what makes it fair), and one truncated on purpose with `text-overflow: ellipsis` or `-webkit-line-clamp` (react-native-web's `numberOfLines`); whether its full text is shown somewhere is a person's call.
- **The finding** names up to 5 elements by role and name, and how many more.

#### 3.6.8 Page title (2.4.2)

- **Each route's** `document.title` is read after its load and the Sample's 3 seconds, since the app sets it in an effect (`useDocument`, `app/src/cloud/publicPages.tsx`).
- **A route fails** when its title is empty, or the same as another route's in the run ("each route has its own title"). Routes are compared by path with the query left out, so `/board?sample` and `/board` are one route.
- **One finding** for all the routes that share a title, listing them ("10 routes share the title Retaskable: /inbox, /board, …"), by section 3.3's rule that a failure on many routes is one finding.

#### 3.6.9 What a person sees, and the tests

- **In the Inbox**: these checks' failures join the sweep's in the run's one `[Scan]` draft, each with its criterion, its routes and, for the element checks, up to 5 elements by role and accessible name (never a selector, as section 3.3's keys). The draft is only in the project's own Inbox. Only the `checks` entries' pass or fail go up (section 3.4), never a route, a name or an element.
- **In the log**: their share of the sweep's time ("A11y: keyboard and layout checks on 12 routes, 21 s").
- **Locally**: `npm run scan:a11y` prints them with axe's rules, each failing check with its routes and elements, so a change to a component can be checked before it merges.
- **Time**: about 2 seconds a route for the walk and its four checks, and about 1 more each for reflow and text spacing. On this repo (`/`, the 10 Sample screens and `scan.pages`, about 12 routes) that's under 50 seconds on top of axe. Section 3.3's two minutes stays the limit; if they push past it, the first trim is the walk's 60 presses, not a check.
- **Tests** (`agent/src/a11yChecks.test.ts`): the judges with plain data (a full round, a walk ending on the page, one cut at 60 with candidates after it, a stuck element, a circle of three, a circle Escape leaves, a ring in `box-shadow`, a border-only change, nothing changed, four of five points covered, all five covered, a page 412 px wide, an inner sideways scroller, an element clipped only once spaced, one clipped before, one with an ellipsis, two routes with one title, an empty title); and the collectors against fixture pages served from `127.0.0.1` by the test itself, in the Chrome the sweep uses, skipped with a note where there's none.

