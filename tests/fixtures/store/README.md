# Store fixtures

`answers.json` holds what the real store answers to the ops the agent uses, and the request each answer was given
to. It is generated, not written by hand: `scripts/store-fixtures/generate.mts` runs Retaskable.com's
`shared/backlogOps` (the pure `runOp`, in memory) on a seeded state, so

- every `value` is what the store really gives (`run_take`, `run_beat`, `run_done`, `merge_enqueue`, `merge_next`
  for a piece and an item step, `merge_beat`, `merge_open`, `merge_result`, `merge_withdraw`, `health`,
  `settings_get`, `intent_*`, `draft_claim_next`, `ci_claim`, `ci_finish`);
- every request in it passed the store's own argument checks (`str`, `optStr`, `oneOf`, ...); one the store refuses
  makes the generator throw, and refused cases keep the code (`error`);
- `limits` are found by probing the store (`merge_result_reason`: the most UTF-16 units of a `reason`);
- `source.commit` names the Retaskable.com commit it came from.

The Rust tests (`src/backlog/store_fixtures/tests.rs`, and the fake stores in `src/orchestrator/driver/tests/runs.rs` and
`src/backlog/merge/tests.rs`) answer with these instead of hand-written JSON, parse them with the real types, and check
that what the client sends is a subset of a request the store took (`assert_sent`).

## Refreshing

```sh
git clone https://github.com/Retaskable/Retaskable.com /path/to/retaskable-com   # or `git pull` in a checkout
cd /path/to/retaskable-com && npm ci --ignore-scripts
cd /path/to/this-repo
RETASKABLE_COM_DIR=/path/to/retaskable-com scripts/store-fixtures/generate.sh
git diff tests/fixtures/store
```

A diff is the store having changed: fix the Rust client or the tests it breaks, then commit the file. To add an op, add
a `take(name, backlog, caller, op, args)` to `generate.mts` and a test that feeds `answer(name)` to the client and calls
`assert_sent(name, ...)`.

## Checking it is current

`cargo test` is offline: `the_file_is_what_the_store_code_gives` says it is skipped and passes. With
`RETASKABLE_COM_DIR` set to an up to date checkout (`npm ci --ignore-scripts` run there) it regenerates the file and fails
on any difference from the committed one, so a CI step needs only that variable.
