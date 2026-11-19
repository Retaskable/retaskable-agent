// Writes tests/fixtures/store/answers.json: what the REAL store (Retaskable.com's shared/*.ts, run in memory through
// `runOp`) answers to each op the agent uses. Every request in it is checked by the store's own argument checks as it
// runs (a bad one throws here), so the file holds only requests the store accepts and answers it really gives.
// Run: RETASKABLE_COM_DIR=<checkout> scripts/store-fixtures/generate.sh   (see tests/fixtures/store/README.md)
import { writeFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { pathToFileURL } from "node:url";

const dir = process.env.RETASKABLE_COM_DIR;
if (!dir) throw new Error("set RETASKABLE_COM_DIR to a Retaskable.com checkout");
const load = (f: string) => import(pathToFileURL(`${dir}/shared/${f}.ts`).href);
const { emptyState, runOp } = await load("backlogOps");
const kit = await load("backlogOpsTestKit");
const { alice, bob, site, agent, SHA, seeded, planned, plan } = kit;

type Case = { op: string; args: unknown; value?: unknown; error?: string };
const cases: Record<string, Case> = {};
const limits: Record<string, number> = {};

/** Runs `op` on the backlog `b`, keeps its request and answer under `name`, and gives the answer. */
const take = (name: string, b: any, caller: unknown, op: string, args: unknown = {}) => {
  const value = b.op(caller, op, args).value;
  cases[name] = { op, args, value: value === undefined ? null : value };
  return value as any;
};
/** The same, for a request the store refuses: the code it refuses with is kept. */
const refuse = (name: string, b: any, caller: unknown, op: string, args: unknown) => {
  try {
    b.op(caller, op, args);
  } catch (e: any) {
    cases[name] = { op, args, error: e.code };
    return;
  }
  throw new Error(`${name}: the store took what it should refuse`);
};
/** A backlog with one piece's merge turn held by alice: where the checks of a result are reached. */
function heldTurn() {
  const b = planned(plan(1));
  const build = b.value(alice, "run_take", { agent, piece: "store#1" });
  b.op(alice, "run_done", { token: build.token, outcome: "pr_opened", result: { branch: "retaskable/store-piece-1", prUrl: "https://github.com/o/r/pull/1", prNumber: 1 } });
  b.op(alice, "run_done", { token: b.value(alice, "run_take", { agent, piece: "store#1" }).token, outcome: "green" });
  return { b, token: b.value(alice, "merge_next", { base: "main", piece: true, agent }).turn.token as string };
}
/** The most UTF-16 units a merge_result's `reason` may hold, found by asking the store. */
const reasonLimit = () => {
  let [lo, hi] = [1, 5000];
  while (lo < hi) {
    const mid = Math.ceil((lo + hi) / 2);
    const { b, token } = heldTurn();
    try {
      b.op(alice, "merge_result", { token, outcome: "sent_back", reason: "x".repeat(mid) });
      lo = mid;
    } catch (e: any) {
      if (e.code !== "bad_request") throw e;
      hi = mid - 1;
    }
  }
  return lo;
};

// This computer as the Rust agent names itself (what its tests send; src/backlog/store_fixtures/tests.rs keeps them the same).
const computer = { machine: "box", environment: "Production", baseBranch: "main" };
const claimAgent = { ...computer, engines: ["claude"], uses: { plan: "claude", build: "cursor", fix: "cursor", look: "claude" } };
const pr = { number: 7, url: "https://github.com/o/r/pull/7" };

// Plan, build, watch and merge runs of one item, in pieces.
{
  const b = seeded(undefined, true);
  take("settings_get_empty", b, alice, "settings_get");
  take("settings_set", b, site, "settings_set", { baseVersion: 0, patch: { baseBranch: "dev" } });
  take("settings_get_saved", b, alice, "settings_get");
  take("run_take_nothing_for_plan_when_busy", planned(), alice, "run_take", { agent, kinds: ["plan"] });
  const t = take("run_take_plan", b, alice, "run_take", { agent: claimAgent, kinds: ["plan"] });
  take("run_beat", b, alice, "run_beat", { tokens: [t.token] });
  take("run_done_planned", b, alice, "run_done", { token: t.token, outcome: "planned", result: { pieces: plan(2) } });
  for (const i of [1, 2]) {
    const name = (what: string) => (i === 1 ? what : `${what}_${i}`);
    const build = take(name("run_take_piece_build"), b, alice, "run_take", { agent: claimAgent, piece: `store#${i}` });
    take(name("run_done_pr_opened"), b, alice, "run_done", { token: build.token, outcome: "pr_opened", result: { branch: `retaskable/store-piece-${i}`, prUrl: `https://github.com/o/r/pull/${i}`, prNumber: i } });
    const watch = take(name("run_take_piece_watch"), b, alice, "run_take", { agent: claimAgent, piece: `store#${i}` });
    take(name("run_done_green"), b, alice, "run_done", { token: watch.token, outcome: "green", result: {} });
  }
  const m = take("merge_next_piece", b, alice, "merge_next", { base: "main", piece: true, agent: { machine: "my-pc" } });
  take("merge_next_piece_again", b, alice, "merge_next", { base: "main", piece: true, agent: { machine: "my-pc" } });
  take("merge_next_piece_waiting", b, bob, "merge_next", { base: "main", piece: true, agent: { machine: "other-pc" } });
  take("merge_open_held", b, alice, "merge_open", {});
  take("merge_beat_held", b, alice, "merge_beat", { token: m.turn.token, phase: "ci" });
  take("merge_beat_lost", b, alice, "merge_beat", { token: "nope", phase: "ci" });
  take("merge_result_sent_back", b, alice, "merge_result", { token: m.turn.token, outcome: "sent_back", reason: "x".repeat(300) });
  const m2 = take("merge_next_piece_second", b, alice, "merge_next", { base: "main", piece: true, agent: { machine: "my-pc" } });
  take("merge_result_merged", b, alice, "merge_result", { token: m2.turn.token, outcome: "merged", mergeCommit: SHA });
  take("merge_open_none", b, alice, "merge_open", {});
  take("merge_next_nothing", b, alice, "merge_next", { base: "main", piece: true, agent: { machine: "my-pc" } });
  const held = heldTurn();
  refuse("merge_result_reason_too_long", held.b, alice, "merge_result", { token: held.token, outcome: "sent_back", reason: "x".repeat(301) });
  limits.merge_result_reason = reasonLimit();
}

// An item step's merge: an entry the computer queues, then its turn.
{
  const b = seeded(["a1", "b1"], true);
  take("merge_enqueue_item", b, alice, "merge_enqueue", { item: "store", base: "main", head: "h1", pr, land: "retaskable", holdsBranch: true });
  take("merge_next_item", b, alice, "merge_next", { base: "main" });
  take("merge_next_item_waiting", b, bob, "merge_next", { base: "main" });
  take("merge_withdraw", b, alice, "merge_withdraw", { item: "store" });
}

// Intent rows.
{
  const b = seeded(undefined, true);
  const { id } = take("intent_request", b, alice, "intent_request", { kind: "ask", args: { question: "Show me the pricing page" } });
  take("intent_list", b, alice, "intent_list", { kinds: ["ask"] });
  take("intent_claim", b, bob, "intent_claim", { id, machine: "my-pc", can: true });
  take("intent_beat", b, bob, "intent_beat", { id });
  take("intent_finish", b, bob, "intent_finish", { id, status: "done", result: { url: "https://x/pr/1" } });
  take("intent_list_none", b, alice, "intent_list", { kinds: ["nothing"] });
}

// A draft a computer claims.
{
  const b = seeded(undefined, true);
  take("draft_queue", b, alice, "draft_queue", { titles: ["Dark mode"], request: "Make it dark" });
  take("draft_claim_next", b, bob, "draft_claim_next", { agent: computer, engines: ["claude"] });
  take("draft_claim_next_none", b, bob, "draft_claim_next", { agent: computer });
}

// The computer's health report (pools-on-server.md section 5.1).
{
  const b = seeded(undefined, true);
  take("health", b, alice, "health", { running: [], limits: {}, spend: {}, paused: false, draining: false, obeys: true, agent: computer, project: "p", can: ["repo", "engine"] });
}

// The agent-run CI's leases, with a results store of their own as backlogCiOps.test.ts keeps.
{
  let state = emptyState();
  const facts = new Map<string, any>();
  const rows = { get: (k: string) => facts.get(k) ?? null, all: () => [...facts.values()] };
  const b = {
    op: (caller: unknown, name: string, args: unknown) => {
      const r = runOp(state, name, args, { now: 1_000_000, caller, isOnline: () => false, newToken: () => "t", facts: rows });
      state = r.state;
      for (const row of r.facts) facts.set(row.kind, row);
      return r;
    },
  };
  const key = { sha: SHA, context: "ci/agent/pr/ci", event: "pull_request", pr: 7 };
  take("ci_claim", b, alice, "ci_claim", { ...key, computer: "alice-pc" });
  take("ci_finish", b, alice, "ci_finish", { sha: SHA, context: key.context, event: "pull_request", pr: 7, result: "passed", minutes: 2.5, computer: "alice-pc" });
}

const commit = execFileSync("git", ["-C", dir, "rev-parse", "HEAD"]).toString().trim();
const out = process.argv[2];
if (!out) throw new Error("usage: generate.mts <output file>");
writeFileSync(out, JSON.stringify({ source: { repo: "Retaskable/Retaskable.com", commit }, limits, cases }, null, 2) + "\n");
