## 9. What the app and site ask of the computer

The agent has no UI. The only screen is the Expo app in `app/` ([expo.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/expo.md)): retaskable.com serves its web build, phones run it in Expo Go, and nothing on this computer serves it (no-local-ui, [tray.md](../tray.md) section 6.1). The app's screens, where each lives and the words on them (the everyday words of [plain-language.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/plain-language.md) section 5) are the app's own specs, not this one. The agent's port serves only `GET /api/health` and the computer's own settings page (section 8).

This section is what the computer still does for those screens: what it works out and reports, and the work it runs when a command from the site asks. Each of the old dashboard's tabs, menus and dialogs (the Roadmap board, the Inbox and its filters, the bell, the chat window, the usage meters, the Profile dialog, the Flow view, the Releases tab's editor, the header and its chips) now belongs to the app and the hub, and is not described here.

**Words a model writes for the person** follow the same plain-language rule: `prompts/drafter.md`, `implementer.md`, `reviewer.md` and `assistant.md` each end with a short closing rule, so a draft's summary, a question, a try-out's what and steps, and a chat answer are short everyday words, while the spec files the drafter writes stay precise ([plain-language.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/plain-language.md) section 7).

**Images.** Every command that sends text to Claude (Ask Claude, Add spec, Send back, question answers, playtest problems, and Continue and Start over notes) can carry images. The agent saves them under `.retaskable/attachments/` and adds their paths after the text (section 8).


- [9.1 What the computer works out for a project's settings](09.1-what-the-computer-works-out-for-a-projects-settings.md)
- [9.2 What the computer reports](09.2-what-the-computer-reports.md)
- [9.3 What the computer does when a command asks](09.3-what-the-computer-does-when-a-command-asks.md)
- [9.3.1 Ideas, in this Rust agent](09.3.1-ideas.md)
