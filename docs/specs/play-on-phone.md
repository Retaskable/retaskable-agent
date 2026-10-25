# Play on Phone — Spec

Status: v0.3. Builds on [agent.md](agent.md) sections 4.2, 8, 9 and 10. It installs a project's own Android app (a Capacitor project in its repo); Retaskable's own Capacitor app in `mobile/` is gone, so Retaskable's repo has nothing for it to install. Trimmed for the agent: the buttons, dialogs and screens are the Expo app's, and the agent takes no command on its own port, only through the cloud link.

Theme: UI

## 1. What it is

A project's Android app is a Capacitor project in its repo (say `phone/`). Without this, trying it on a phone means running its sync (`npm run sync -w phone`), opening Android Studio and pressing Run. **Play on phone** does that from Retaskable: the agent notices an Android phone plugged into this computer with USB debugging on, and offers **Play on phone** (the app shows the button), which builds the app from the base branch and installs and launches it on that phone.

- **Seeing the phone is automatic. Installing never is.** The agent lists connected phones by itself, but nothing is installed until you press the button, and the first time for each phone, until you've read the warning (section 4) and said yes.
- It's a fixed command from the config, run by the orchestrator's code, never something an agent or the website chose. It arrives as the cloud link's `phone_play` command (section 6) and nowhere else: the agent's port takes no command.

## 2. Setting it up

- An optional config section, `phone`, set in the project's settings (the app's **Checks and Play** step has the **Play on phone** switch, **app folder**, **phone build** and **app's item**):

  ```json
  "phone": { "appDir": "phone", "buildCommand": "npm run sync -w phone", "appId": "com.example.app", "itemId": "27" }
  ```

  - `appDir`: the Capacitor project's folder, relative to the repo root (it has `capacitor.config.*` and an `android/` folder).
  - `buildCommand`: builds the web app and copies it into the Android project. Run through the shell from the repo root, like `playCommand`.
  - `appId`: the Android package, read from `capacitor.config.*` by Setup's scan; shown beside the switch, and what **Uninstall and install** removes.
  - `itemId` (optional): the backlog item the app belongs to. While that item isn't `done`, the warning says the app is work in progress (section 4).
  - Saving checks that `appDir` is a relative path inside the repo, `buildCommand` isn't empty and `appId` looks like a package (`com.example.app`).
- Setup's scan of the repo on the computer suggests it (`suggestedPhone`, sent up as a derived result) when the repo (or one of its npm workspaces) has a `capacitor.config.*` beside an `android/` folder, with `buildCommand` from that workspace's `sync` script if it has one (`npm run sync -w <workspace>`), else `npx cap sync android` in that folder (`cd <folder> && npx cap sync android`). A new setup takes the suggestion; a saved one keeps its own, and the switch turns the suggestion on. Without a `phone` section nothing else in this spec shows up, except one hint: when the repo has an app Setup would suggest and adb is found, the agent still watches for phones (section 3), and while one is plugged in `phone.status` says "A phone is plugged in. To install this repo's Android app on it, turn on Play on phone." (with no phones listed), so it can't be picked. So a config saved before the app existed finds out about it.

## 3. Finding adb and the phone

- **Finding adb**, in this order: `$ANDROID_HOME/platform-tools/adb`, then `$ANDROID_SDK_ROOT/platform-tools/adb`, then `adb` on `PATH`, then the SDK Android Studio installs by default (`%LOCALAPPDATA%\Android\Sdk` on Windows, `~/Library/Android/sdk` on macOS, `~/Android/Sdk` on Linux), with `adb.exe` on Windows. Paths are joined with `path` APIs, so it's the same code on Windows, Linux and macOS. It's looked up again when the agent starts and when Setup is saved.
- **Watching for phones**: while a `phone` section exists and adb was found, the agent runs `adb devices -l` every 5 seconds (outside any sandbox, with a 10-second timeout, never while a previous run is still going) and keeps the result in State as `phone.phones`. It's cheap: adb's server answers from memory once started.
- **Only authorised phones are offered.** A line in the `device` state is a phone you can install to. `unauthorized` (the phone hasn't accepted this computer's key), `offline`, `recovery`, `sideload`, `bootloader` and anything else are never offered; the first two get a hint instead (section 5). Emulators (`emulator-*` serials) count like phones.
- Each phone in State is `{ serial, model, state, warned }`, with the model from `adb devices -l`'s `model:` field (underscores as spaces), e.g. "Pixel 8", or the serial when it has none. `warned` says the warning (section 4) was accepted for it. Beside the list, State's `phone` holds `status` (below), `appId`, `wip` (the app's item isn't done) and `run`, the last or current Play on phone: `{ serial, model, step, message?, at }`, with `step` one of `building`, `installing`, `done`, `failed`, `incompatible` or `stopped`.
- **When something's missing**, State's `phone.status` says what, and the app shows it instead of the button:
  - No adb: "adb wasn't found. Install Android's SDK Platform-Tools and set ANDROID_HOME, or put adb on your PATH."
  - adb found, but the build needs the SDK: a build that fails because Gradle can't find the SDK shows its last output, and the note "Android's SDK wasn't found: set ANDROID_HOME, or open `<appDir>/` in Android Studio once." When `ANDROID_HOME` and `ANDROID_SDK_ROOT` are both unset and adb was found in an SDK's `platform-tools/`, the build gets `ANDROID_HOME` set to that SDK.
  - `adb devices` fails or times out: its error, retried at the next poll.
  - Gradle's wrapper can't be run: a `cap run android` that fails with "'gradlew' is not recognized" (Windows' cmd.exe not finding `gradlew.bat` in `<appDir>/android/`) shows its last output, and the note "Windows couldn't run Gradle's wrapper (`<appDir>/android/gradlew.bat`). Check it exists, or run `npx cap sync android` in `<appDir>/` to add it." With section 5's environment fix this shouldn't happen, so it points at a missing wrapper rather than the environment.

## 4. The warning

The first time Play on phone is asked for a phone (by serial), the app shows a dialog before anything runs, and the agent refuses an unconfirmed first install with the same text (the text is `installWarning` in `shared/phone.ts`):

> **Install on Pixel 8?**
> - This is a **debug build**, signed with this computer's debug key, not the Play Store's.
> - If the phone already has `com.example.app` (the `appId`) signed with a different key (from the Play Store, or another computer), Android won't install over it. Retaskable will offer to **uninstall it first, which deletes the app's data on the phone** (you'll be signed out, and its notification settings are lost). It never does that without asking.
> - **USB debugging gives this computer broad access to the phone**: it can install apps, read files and run commands on it. Turn it off in the phone's Developer options when you're done, and only accept the "Allow USB debugging?" prompt for computers you trust.
> - *(While the app's item isn't done)* The app is **work in progress**: parts of it may not work yet.
>
> **Install** · **Cancel**

- Confirming remembers that phone's serial as warned in `.retaskable/phones.json` (`{ "warned": [serials] }`; the file stays on the machine, though each plugged-in phone's `warned` goes up in the snapshot with it, section 6), and installs. Later requests for the same phone go straight to installing; the dialog comes back if that file is deleted.
- **A different signature.** If the install fails with `INSTALL_FAILED_UPDATE_INCOMPATIBLE`, Play on phone stops (`step: "incompatible"`) and the app says so in a dialog, with **Uninstall and install** (and its own warning that the app's data on the phone will be deleted) and **Cancel**. Only that confirmed request runs `adb uninstall`. It's never the default.

## 5. The button and what it runs

- **Where it shows**: the Expo app's Run controls, as **Play on phone** or one **Play on <model>** per phone, greyed out with a reason when none is ready. The agent sends the reasons in `phone.status` and each phone's `state`: "Plug in an Android phone with USB debugging on", "A phone is connected but hasn't allowed USB debugging from this computer. Unlock it and accept the prompt." (`unauthorized`), "The phone's connection is offline. Unplug it and plug it in again." (`offline`).
- **What it runs**, in order, all fixed:
  1. In the base build's checkout, the same one Play main uses (the planner's worktree, moved to `origin/<baseBranch>` first; the Retaskable checkout itself when it's building itself, agent.md section 4.2): the dependencies' install when they're missing or out of date, as before a gallery shoot (agent.md section 6.8), then `buildCommand`.
  2. In `<appDir>`: Capacitor's `cap run android --target <serial> --no-sync`, which builds the debug APK with Gradle, installs it on that phone and launches it. It's the checkout's own CLI (`node_modules/@capacitor/cli/bin/capacitor` in `<appDir>`, else at the root) run with node, so there's no shell (`npx` is a `.cmd` on Windows). `<serial>` is passed as its own argument, and only if it's one of the serials in `phone.phones` in the `device` state at that moment (checked again after the build, as it may have been unplugged meanwhile).
  Each step has a 30-minute timeout.
  - **The environment for both steps** is the checkout's (as for a gallery shoot), with `ANDROID_HOME` as in section 3, and without `NoDefaultCurrentDirectoryInExePath`. Claude Code sets that variable in its shells and the agent inherits it; it stops Windows' cmd.exe looking in the current folder for a command. Capacitor runs Gradle as `./gradlew`, which on Windows becomes `gradlew` run through cmd.exe in `<appDir>/android/`, so with it set the install fails with "'gradlew' is not recognized as an internal or external command" even though `gradlew.bat` is there. The variable is removed whatever its case (Windows' environment names are case-insensitive, so `nodefaultcurrentdirectoryinexepath` goes too), and only for Play on phone's own commands (these two steps, and the `adb uninstall` before them): the agent's own environment and everything else it runs are unchanged. On Linux and macOS the variable means nothing and removing it changes nothing.
- **Nothing else can be passed.** The `phone_play` command takes `{ serial, confirmed?, uninstall? }` and nothing more: no command, no adb arguments, no path. A serial that isn't a currently authorised phone is refused. The first install to a phone without `confirmed: true` is refused with the warning's text. `uninstall: true` (with `confirmed: true`) runs `adb -s <serial> uninstall <appId>`, then steps 1 and 2 again (the base checkout may have moved since).
- **While it runs**, `phone.run` shows the step ("Building…", "Installing on Pixel 8…"). `phone_stop` ends the running step. A failure keeps the step's last 30 lines of output on the machine; a launch is `done`. It's refused while the base checkout is busy (a backlog sync, a gallery shoot, or another Play on phone), and a backlog sync, the baseline checks, a gallery shoot and Play main wait for it, as for Play main. Once the app has launched on the phone, it's done: Retaskable doesn't keep a process running for it.
- **No automated step ever runs it.** No agent, chore, playtest or Ask Claude action installs to a phone. Ask Claude doesn't get a `play_on_phone` action either.

## 6. From retaskable.com

Built, play-from-site slice 2. It was a request ([tray.md](tray.md) section 5.1) until 2026-10-22; since then it runs when it arrives, with its checks ([cloud.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/cloud.md) section 6.4).

- **What goes up.** State's `phone` is in the snapshot (`toSnapshot` in `shared/cloud.ts`, sent whole in a patch when it changes): the phones adb lists with their serial, model, state and `warned`, the app id, `wip`, `status` and the last run, without a failed run's output (its `message`, which stays on the machine like the staging smoke's). So the serials of the phones plugged in are seen by everyone on the project.
- **The commands.** `phone_play` `{ serial, confirmed?, uninstall? }`, nothing more (the serial is letters, digits, `.`, `:` and `-`), and `phone_stop`. Both are on the cloud link's list ([cloud.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/cloud.md) section 6). The agent checks `phone_play` itself (`checkPlay` in `agent/src/phone.ts`: set up, the serial a phone in the `device` state, the warning or uninstall confirmed), so an unconfirmed first install is refused with the warning's text, then runs `phone.play`; the `ack` comes once it has started, and the build's steps show from the snapshot.
- **On the site.** Showing the warning, the buttons and their waiting lines is the Expo app's, from the snapshot's `phone` and `warned`.

## 7. Acceptance

- With no `phone` section, nothing changes, apart from the hint to turn it on while a phone is plugged in and the repo has an Android app. With one and no adb, `phone.status` says adb is missing and how to fix it.
- Plugging in a phone with USB debugging on lists it in `phone.phones` within about 5 seconds, and unplugging it drops it. A phone that hasn't accepted the computer shows the hint, never as installable.
- Plugging a phone in installs nothing. The first request for a phone without `confirmed` is refused with the warning; a cancelled dialog runs nothing.
- A confirmed request builds from the base branch and the app opens on the phone. The second request for the same phone skips the warning.
- A phone with a Play Store install gets the signature message, and nothing is uninstalled until **Uninstall and install** is pressed.
- A `phone_play` with an unknown serial is refused, and the port binds to `127.0.0.1` and takes no command (agent.md section 8).
- The same code works on Windows, Linux and macOS, finding adb through `ANDROID_HOME` or `PATH`.
- On Windows, with the agent started from a Claude Code shell (so `NoDefaultCurrentDirectoryInExePath=1`), a confirmed request runs Gradle and the app opens on the phone. Tests check that the build and install steps' environment has no `NoDefaultCurrentDirectoryInExePath` in any case while keeping the rest, and that `installFailure` turns "'gradlew' is not recognized" into section 3's note.
