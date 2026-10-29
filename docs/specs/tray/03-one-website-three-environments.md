## 3. One website, three environments

The environments are the cloud's: the same code, each with its own data, sign-in and secrets, and nothing crossing between them ([cloud.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/cloud.md) section 9, [environments.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/environments.md) section 3). What the agent holds to:

- **The agent is the only thing linked between environments.** It keeps one connection and one token per environment it is linked to. A project linked to two environments by the same agent is two unrelated projects, one in each.
- **Nothing crosses through it.** The agent copies no account, organisation, invitation, public page, gallery, release note, push subscription or audit row from one environment to another, and sends each environment only that environment's own project.

In the Rust agent each cloud has its own `DeviceTokens` and `Connector` (`src/link/`, `src/connector/`), and its refresh token is stored under that cloud's URL.
