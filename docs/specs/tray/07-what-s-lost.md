## 7. What's lost

- **No UI without a reachable retaskable.com.** Offline, or with the environment down, there's only the tray menu: the link state, Pause, Quit and the shared backlog's approval. The agent fails closed when it can't reach the shared backlog; the site shows the work again once connected. For a maintainer, running retaskable.com locally ([cloud.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/cloud.md) section 9) is the offline UI.
- **Setup needs an account.** A first run means signing in to some environment with GitHub, even to try it alone. Local is an option for that too.
- **An item's log after its session.** retaskable.com shows a session's events only while it runs (they're streamed, never stored); once it ends, the item's Transcript says older logs aren't shown. The files stay on the machine, in the item's run folder, for anyone who looks there. Decided 2026-10-08 (6.1.5).
- **Some things take a second step**: a changed command runs on a computer only once its owner approves it on the site ([cloud.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/cloud.md) section 6.4).
