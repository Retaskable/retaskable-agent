# Name a project's organisation by slug (the agent's part)

Status: built in this Rust agent 2026-11-19. The hub's half, the site and the store migration are in [Retaskable.com's organisation-by-slug.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/organisation-by-slug.md) (sections 3.1 and 3.2's record and migration), which this follows; where they differ on a message's shape, theirs wins.

## 1. What it is

An organisation's numeric id belongs to one cloud: the same organisation is 3 on production and 2 on staging, and 2 on production is somebody else's. Everywhere the agent carries the organisation of a project to a cloud it carries the **slug**, which is the same on every cloud, and each hub turns the slug into its own id. The agent no longer keeps one id per cloud (the TypeScript agent's `organisationFor`, and this agent's `organisationsBy` mapping in `store_organisation.rs`).

## 2. Decisions

- **[Decided]** (the owner, 2026-11-18) The shared store's settings record names the organisation by `organisationSlug` only. This agent reads `organisationSlug` from it and ignores any `organisationId` in it (a record from before the migration holds another cloud's number).
- **[Decided]** Every `hello`'s `project` carries `organisationSlug` (the hub's `z.string().max(100)`), to every cloud, whichever cloud the agent is talking to.
- **[Decided]** The id stays inside one cloud. The agent's own cloud (the first it connects to, `cloud.url`) is the only one that gets `organisationId` as well, its own id for the organisation, as a fallback for a hub that still reads only the id (the hub keeps accepting it for a window of at least two agent releases, then drops it; where it reads both, the slug wins). No other cloud is ever sent an id.
- **[Decided]** `link_project` takes `organisationSlug` (the site sends it while the hub's `org-slug-hub` flag is on), and still takes an older site's `organisationId`, as the id of the agent's own cloud only when the command arrives over that cloud's connection; where both are named, the slug wins.

## 3. Behaviour

### 3.1 The slug a hello names

For one project, in this order:

1. the store record's `organisationSlug`, when the record names one;
2. the slug the agent's own cloud's `GET /api/agents/organisations` answer gives for the id in the project's config (`organisationId` in `config.json`);
3. the `organisationSlug` kept in `config.json` (written by `link_project`, below).

None is sent while none of the three is known; the hello goes again when the record or the answer arrives or changes (so a project linked to staging and production sends both the same slug, and no hello waits for the other cloud's answer, which this agent no longer reads for the purpose).

The `organisationId` for the agent's own cloud only is the own cloud's id for the record's slug (its answer's organisation with that slug; none when the answer has no such organisation, and the config's own id, as it was kept, while the answer hasn't arrived), else the config's `organisationId`. An older hub that reads only the id gets it; a hub that reads the slug lets it win. The answers of the other clouds are no longer asked for, and never used.

### 3.2 The record and the config

The settings record's `organisationSlug` is read on each `settings_get` (the loop's `settings_values`); a change wakes the connections, which say hello again if theirs differs. `organisationId` in a record is ignored, and a record with only an id leaves the config's own. `config.json`'s `organisationId` stays this cloud's id (what a person fixes by hand, and what the switcher and caps use); a project linked by slug also keeps `organisationSlug` there so its hello can name it before the own cloud has answered.

### 3.3 `link_project`

`organisationSlug` (a string) and `organisationId` (a positive whole number) are the two organisation arguments. A slug is checked against the slugs in the agent's own cloud's organisations answer once it has one: one that isn't there is refused (`That organisation isn't one this computer is linked to: open its page and ask from there.`); the project's config then keeps the own cloud's id for it and the slug. With no answer yet the slug is kept alone (the hello names it; the id is worked out once the answer arrives). An `organisationId` is taken only over the own cloud's connection (over any other cloud's it is dropped: its number is not ours), is checked against the answer the same way once there is one, and keeps the slug the answer gives it. A slug wins over an id. Any other argument is still refused, as before.
