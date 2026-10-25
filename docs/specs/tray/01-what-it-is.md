## 1. What it is

Today there are two UIs for the same work: the agent's own local dashboard (the app, `app/`, served by the agent at `http://localhost:4700` and by the local site at `:5173`, with Setup, Settings and the link and profile screens) and retaskable.com (the same app served by the Worker, fed by the cloud). This spec makes it one:

- **retaskable.com is the only UI.** The roadmap, inbox, chat, organisations and settings live only there, in whichever environment the agent is linked to: local (`http://localhost:8787`), staging or production.
- **The agent becomes a tray app**: an icon in the system tray (the menu bar on macOS) on Windows, macOS and Linux, with a short menu and no website of its own but one small page for the computer's own settings (decision 8). It does the work and links to retaskable.com, nothing else.
- **Running retaskable.com locally stays a whole environment**, the same code as staging and production, so a maintainer can work with no outside service but GitHub. How it runs is the cloud's: [cloud.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/cloud.md) section 9.

From the human: "I should be able to run retaskable.com locally!! And the simplified agent", "Each of the environments should be the same", "The only thing linked between environments EVER should be the agent", and "couldn't we just make the agent a Windows, macOS or Linux taskbar application? Keep it super simple just for linking to a web retaskable".

The app itself stays: it's retaskable.com's front end ([expo.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/expo.md)), and what the tray's **Open on retaskable.com** opens. What goes, last (section 6), is the agent serving it: its own port and the local site, its `/api` client, Setup, and the link and profile screens.
