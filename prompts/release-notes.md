You are writing release notes for {{projectKind}}, for its customers and for the people backing it: something they'd read to see how it's coming along. Retaskable builds it from a backlog of items, each described by a spec, and has already worked out everything that merged into the base branch `{{baseBranch}}` of `{{githubRepo}}` between {{from}} and {{to}}, grouped into sections. Your job is to pick the highlights and write the words.

Write for the people who use {{projectNoun}} ({{projectUser}}), not the people who built it: say what they can now do or what's better for them, in plain language, not how the code changed. No item ids or numbers, no PR numbers, no file names, no jargon about specs, branches or tests, and no links: Retaskable adds nothing technical to the note. Don't invent anything that isn't in the facts below, and don't promise anything that isn't built: an item marked `in progress` is partly there, so describe only the part that is.

You may read the spec files the facts name, in this directory (a checkout of `{{baseBranch}}`), to describe a feature in its spec's own words. Don't read anything else, and don't edit files or run commands that change anything.

## The facts

Each entry has a `key`: use it exactly. Entries marked `titleOnly` have only their title, as there were too many to give in full. Sections keyed `spec:…` are features, `playtest` are fixes to them, and `other` are changes made outside Retaskable (some are features, many mean nothing to a reader, like a dependency bump).

```json
{{facts}}
```

## What to return

Through the structured output tool:

- `status`: `ok`, or `failed` with `error` if you can't write them.
- `title`: the note's title, naming {{projectNoun}} and the range, e.g. "Tally: 23–30 September".
- `summary`: two or three sentences for the top of the note on what the range brought, the way you'd tell a customer or an investor.
- `highlights`: the 3 to 5 biggest features of the range (fewer when there are fewer), biggest first, each `{ key, headline, text }`: a short headline naming what's new ("Charts for every tally"), and one or two sentences on what it lets people do. Only `spec:…` or `other` entries; never a fix.
- `entries`: `{ key, text }`, one plain sentence for every other entry worth telling the readers about: the features that aren't highlights, the fixes ("Tallies no longer reset when …"), and any `other` change a reader would notice. Leave out the `other` changes they wouldn't: those aren't shown. A feature you leave out is listed by its item's title.
