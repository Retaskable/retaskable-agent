# Repo Map — Spec

Status: v0.1, proposed 2026-11-04. Builds on [agent/06.2-implementer.md](agent/06.2-implementer.md) ("Where to start") and [agent-telemetry.md](agent-telemetry.md) section 5 (`start-report`). This is a **Rust-only** feature: the TypeScript agent is being retired, so there is no `agent/` counterpart, and Retaskable.com's specs don't carry it. That makes this file the source of truth for it, unlike the rest of this folder ([README.md](README.md)).

Theme: Implementer sessions
Kind: new

## 1. What it is

A **repo map** is a short, ranked list of the functions and types a project defines: one line per definition, giving its file, its signature and the first line of its doc comment. It goes in a fresh session's prompt next to "Where to start", so the session can see what exists and where before it reads or searches anything.

**Why.** "Where to start" names files. It can't say what is in them, and it says nothing about the code no spec names. Sessions still spend their first turns looking for where something lives:
- On 2026-10-16, implementer sessions took a median of 24 reads and searches (3.7 minutes) before their first edit.
- On 2026-10-20, after "Where to start" (`start-report`, 20 sessions), it was a median of 19 turns out of 54, and 36% of the session's cost, before the first edit. 92% of those turns made a single call, most of them a search or a read to find a name.
- On 2026-10-30, drafters spent 26 of 40 turns before their first write.

Every one of those turns resends the whole conversation. A map of about 1,500 tokens, sent once, answers most "where is X" and "is there already a Y" questions without a turn.

**What it is not.**
- **Not a database or an index on disk.** It is built from the lane's checkout when the session starts, put in the prompt and dropped. Nothing is stored, cached or synced. CLAUDE.md says "There is no DB and no queue", and a map built fresh can never be stale.
- **Not embeddings or vector search.** Section 2.1 explains why.
- **Not a tool the session calls.** It's prompt text. The session's own Read, Grep and Glob are unchanged.
- **Not sent anywhere.** It stays on the computer, in the prompt of a session the computer runs. The server never sees code, and this changes nothing about that.

## 2. Decisions

### 2.1 [Decided] Tree-sitter definitions and references, ranked by a graph

**Chosen:** parse each source file with [tree-sitter](https://tree-sitter.github.io/), take its definitions (with signatures and doc comments) and the names it refers to, build a file graph from "file A refers to a name file B defines", rank it with personalised PageRank, and render the top of the ranking within a token budget. This is the approach Aider's repo map popularised, and it works on a checkout with no build, no language server and no network.

**Rejected:**
- **Embeddings and a vector store (Cursor-style RAG).** This needs a model to embed with, a store to keep the vectors in (a DB, which CLAUDE.md rules out), and re-indexing whenever the code changes. Similarity search answers "code that looks like this text". The question a session actually has is "where is the thing called X, and what does it take". Coding agents have been moving away from this approach towards agentic search plus structure.
- **LSP or SCIP indexes.** These are precise, but each language needs its own server or indexer installed and warmed up on every computer (rust-analyzer alone takes minutes on a cold checkout), and on Windows that's a lot to ask. Tree-sitter grammars compile into the binary.
- **ctags.** This would be another binary to install on every computer, it has no reference data for ranking, and its signatures are uneven.
- **Asking the model to summarise files.** This costs tokens and time on every session, and its output varies from run to run.

### 2.2 [Decided] Built per session, never stored

The map is built once per fresh session, in the lane, after `lanes::prepare` and alongside `start_files::build`. A resumed session, or a restart after running out of turns, gets no map, just as it gets no "Where to start": its transcript already has one.

The build runs on a blocking thread (`tokio::task::spawn_blocking`) under a time limit (section 3.7). Nothing is written to disk. Two sessions in two lanes each build their own map from their own checkout.

### 2.3 [Decided] Rust only

Only this agent builds it. The TypeScript agent's sessions keep "Where to start" without a map. Telemetry (section 5) compares sessions with and without a map, so the comparison is between this agent's sessions with the `repo-map` flag on and off, not between the two agents.

### 2.4 [Decided] Languages: Rust, TypeScript, TSX and JavaScript first

These are the languages of this repo and Retaskable.com. Each language is a grammar crate and a pair of query files (section 3.3). A file in any other language is left out of the map, and nothing else changes. Python and Go are the next candidates. Adding one is a new grammar, two `.scm` files and tests, with no change to ranking or rendering.

### 2.5 [Decided] "What it does" is the first line of the doc comment

A definition's description is the first sentence of its doc comment, cut at 100 characters:
- Rust: `///`, or `//!` for a module.
- TypeScript and JavaScript: the `/** ... */` block right above the definition.

A definition with no doc comment is shown with its signature alone. Nothing is generated or guessed. Projects with good doc comments get a better map, which is one more reason to write them.

### 2.6 [Decided] What counts as a definition

The map shows what code elsewhere can use, because that is what a session searches for.

- **Rust:**
  - Every `fn`, `struct`, `enum`, `trait`, `type`, `const`, `static`, `macro_rules!` and `mod` with a visibility modifier (`pub`, `pub(crate)`, `pub(super)`, `pub(in …)`).
  - Methods with a visibility modifier inside an `impl`, shown as `Type::method`.
  - Every method signature in a `pub` trait.
  - Private items are left out, and so is everything inside a `#[cfg(test)]` module.
- **TypeScript and JavaScript:**
  - Every exported `function`, `class`, `interface`, `type`, `enum` and `const`/`let` (`export …`, `export default …`).
  - The public methods of an exported class. Methods marked `private` or `protected`, or whose names start with `#`, are left out.
  - CommonJS (`module.exports.x =`, `exports.x =`) is counted for `.js`/`.cjs` files.

Test files are never mapped (section 3.2), but their references still count for ranking, since a test that calls something is evidence that it matters.

### 2.7 [Decided] Budget: about 1,500 tokens

The rendered map is at most `repoMap.maxTokens` tokens (default 1,500). Tokens are estimated at 4 characters each, because this agent has no tokenizer and doesn't need an exact count. That gives 6,000 characters by default.

1,500 is a judgement call. Aider defaults to 1,024 for a chat. A session that runs 50 or more turns resends its prompt on every turn, but mostly from cache at 0.1× the price. If the map saves even two turns, it has paid for itself. Section 5 measures this, and the default moves if the numbers say so.

### 2.8 [Decided] Behind the `repo-map` release flag

The prompt section is gated on the `repo-map` flag (the flags client, `src/flags.rs`). Off is today's prompt.

The map is built either way, and its size and build time are logged (section 3.8). That way the cost is visible before any session sees it. With the flag On, the map is added to the prompt.

The flag exists so that `start-report` can compare the two halves (section 5). Once the numbers are in, the flag is removed, either keeping the map or removing it.

### 2.9 [Decided] Built in two pull requests

1. **The map** (section 6.1):
   - the `repo_map` module's discovery, extraction and rendering, ordered by file path with no ranking yet
   - the config key
   - a `retaskable repo-map` command to print it

   Nothing changes in any prompt.
2. **Ranking and the prompt** (section 6.2):
   - the reference graph and personalised PageRank
   - the budget cut by rank
   - the prompt section and its wording, behind the flag
   - `start-report` splitting sessions by whether they had a map

## 3. Behavior

### 3.1 Where the code lives

A new module, `src/repo_map/`. Every file stays under 500 lines (code gates: soft 500, hard 1,000).

| File | What it holds |
|---|---|
| `src/repo_map.rs` | The module root: `pub struct RepoMap`, `pub struct Seeds`, `pub async fn build(root: &Path, seeds: &Seeds, cfg: &RepoMapConfig) -> RepoMap`, `pub fn render(map: &RepoMap) -> String`, and the time limit. |
| `src/repo_map/discover.rs` | Which files to read (section 3.2). |
| `src/repo_map/lang.rs` | `enum Lang { Rust, TypeScript, Tsx, JavaScript }`, choosing a language from a file's extension, and loading each grammar and its compiled queries once (`std::sync::OnceLock`). |
| `src/repo_map/extract.rs` | Parsing one file into its `Def`s and `Ref`s (section 3.3). |
| `src/repo_map/doc.rs` | Doc comment and signature text for a definition node (sections 3.4, 3.5). |
| `src/repo_map/rank.rs` | The graph and personalised PageRank (section 3.6). PR 2. |
| `src/repo_map/render.rs` | The budget cut and the text (section 3.7). |
| `src/repo_map/queries/{rust,typescript,javascript}-{defs,refs}.scm` | The tree-sitter queries, pulled in with `include_str!`. |
| `src/repo_map/tests.rs` and `src/repo_map/fixtures/` | Tests (section 4). The fixtures are small source files, one folder per language. |

**Dependencies** (Cargo.toml):
- `tree-sitter`
- `tree-sitter-rust`
- `tree-sitter-typescript` (provides both `LANGUAGE_TYPESCRIPT` and `LANGUAGE_TSX`)
- `tree-sitter-javascript`

All four must be on the same tree-sitter ABI, and the versions are pinned in Cargo.lock. The grammars are C, compiled by `cc` at build time:
- **Windows:** the MSVC toolchain the Rust install already needs.
- **Linux and macOS:** the system C compiler.

Rust CI, the Woodpecker images and the release build for each platform must all still build. The release binary is expected to grow by a few MB. PR 1 records the actual growth for each platform in its description.

**Public types:**
```rust
pub struct Def {
    pub file: String,          // repo-relative, forward slashes
    pub name: String,          // `build`, `StartFiles`, `Type::method`
    pub kind: DefKind,         // Fn, Method, Struct, Enum, Trait, Type, Const, Macro, Mod, Class, Interface
    pub signature: String,     // section 3.5
    pub doc: Option<String>,   // section 3.4
    pub line: usize,           // 1-based, for ordering within a file
}
pub struct Ref { pub file: String, pub name: String }
pub struct Seeds {
    pub files: Vec<String>,    // "Where to start"'s files, repo-relative
    pub names: Vec<String>,    // identifiers the item's own text mentions (section 3.6)
}
pub struct RepoMap { pub defs: Vec<Def>, pub files_scanned: usize, pub truncated: bool, pub elapsed: Duration }
```

### 3.2 Which files

1. **Listing.** Run `git ls-files -z` in the lane, using `lanes::git_output_in_worktree` as `code_gates.rs` already does. Only tracked files count, so build output, `node_modules` and `target/` never appear, whatever the `.gitignore` says. If git fails, the map is empty (section 3.8).
2. **Language.** The extension decides it:
   - `.rs` is Rust.
   - `.ts`, `.mts` and `.cts` are TypeScript.
   - `.tsx` is TSX.
   - `.js`, `.mjs`, `.cjs` and `.jsx` are JavaScript, parsed with the JavaScript grammar, which handles JSX.

   `.d.ts` files are TypeScript, but only their definitions are kept: they declare and are never referred to by path. Every other extension is skipped.
3. **Skipped:**
   - Anything under a path segment named `vendor`, `third_party`, `node_modules`, `dist`, `build`, `out`, `target`, `generated` or `__generated__`.
   - Files whose first 512 bytes contain `@generated` or `DO NOT EDIT`.
   - Minified files (`.min.js`, or any line over 1,000 characters in the first 4 KB).
   - Files over 256 KB (`MAX_FILE_BYTES`).
   - Files that aren't valid UTF-8.
4. **Tests.** A test file is read for references only. Its definitions are left out of the map. A file counts as a test file if it:
   - matches `code_gates::is_test`, or
   - is a `tests.rs`, or
   - is under a `tests/` segment, or
   - is named `*.test.*` or `*.spec.*`.
5. **Cap.** At most `MAX_FILES` (3,000) files are parsed, taken in `git ls-files` order after filtering. If there are more, `truncated` is set and logged. Real Retaskable repos are well under this. The cap is a backstop for monorepos.

Paths are kept as git prints them: repo-relative, with forward slashes, on every platform. They're joined to the lane root with `Path::join` on each component, never by pasting a string, so `src/a/b.rs` opens correctly on Windows. The rendered map uses the forward-slash form, which matches what "Where to start" prints and what Claude's tools accept on every platform.

### 3.3 Extracting definitions and references

Each language has two queries, run with `tree_sitter::QueryCursor` over the file's tree:

- **`*-defs.scm`** captures `@def.<kind>` on the whole definition node and `@name` on its name node. For Rust, the visibility test (section 2.6) is a `(visibility_modifier)` child required in the pattern. For `impl` methods, the query also captures the `impl`'s `type` field (`@impl.type`), which gives the `Type::method` name. Exportedness in TypeScript and JavaScript is an `export_statement` ancestor (or the CommonJS assignment pattern).
- **`*-refs.scm`** captures `@ref` on identifiers that are used rather than defined:
  - Rust: `identifier`, `type_identifier`, `field_identifier` in call, method-call, field, path, struct-expression, type and macro-invocation positions, plus the last segment of a `use` path.
  - TypeScript and JavaScript: `identifier`, `type_identifier`, `property_identifier` in call, `new`, member, type-annotation, JSX-element and import-specifier positions.

The queries are this repo's own files, not the grammars' bundled `tags.scm`. They need to carry visibility and impl types, and keeping them here means a grammar update can't silently change what the map shows. Each query is compiled once per process, and a query that fails to compile is a bug that the tests catch (section 4).

**Skipped while walking:**
- A definition inside a Rust `mod` item whose attributes include `cfg(test)` is dropped. The walker checks the item's preceding `attribute_item` siblings.
- Any node with `has_error()` drops only the captures inside it. The rest of the file still counts, because tree-sitter recovers from errors and half-written code is normal in a lane.

**Names:**
- References shorter than 3 characters are dropped, and so are references in a per-language stoplist: the language's own common names, such as `new`, `default`, `from`, `into`, `clone`, `len`, `map`, `push`, `get`, `set`, `then`, `length`, `toString` and `Ok`/`Err`/`Some`/`None`. These connect everything to everything and add noise to the ranking.
- A method's own name (`render`, not `StartFiles::render`) is what its references match. The qualified form is only for display.

### 3.4 Doc comments

For a definition node, walk its previous siblings:
- Skip Rust `attribute_item`s (`#[derive]`, `#[cfg]`, `#[serde]`) and TypeScript decorators.
- Collect contiguous doc-comment nodes:
  - Rust: `line_comment` nodes carrying an `outer_doc_comment_marker`, or `block_comment` starting `/**`.
  - TypeScript and JavaScript: a `comment` starting `/**`.
- Stop at the first node of any other kind, or at a blank line between comment and definition. A comment separated by a blank line is a file note, not a doc comment.

A Rust module's description comes from its file's leading `//!` lines.

From the collected text:
1. Strip the comment markers (`///`, `//!`, `/**`, `*/`, a leading `*` on each line).
2. Drop JSDoc tag lines (`@param`, `@returns`, …).
3. Join with spaces and collapse whitespace (`text::collapse_whitespace`).
4. Take the first sentence: up to the first `. ` or `.\n`, or the end. Don't split inside backticks or after `e.g.`/`i.e.`.
5. Cut to 100 characters with `text::clip`.

Markdown is kept as written: backticks, `[`links`]` and all.

### 3.5 Signatures

A definition's signature is its source text from the node's start to the start of its body, with whitespace collapsed. Attributes and doc comments aren't part of it.

- Rust `fn`: up to the `body` block. For example, `pub async fn build(git_bin: &Path, root: &Path, base_ref: &str, item: &Item) -> StartFiles`.
- Rust `struct`/`enum`/`trait`/`impl`: the header only, up to `{`, `(` or `;`, so `pub struct StartFiles`. Fields and variants are left out. They cost tokens, and the session reads the file when it needs them.
- Rust `const`/`static`/`type`: the whole item without the value after `=`, so `pub const MAX_FILES: usize`.
- TypeScript and JavaScript `function` and methods: up to the body, with return type annotations kept. Arrow functions assigned to an exported `const` show as `export const name = (params) => ReturnType`.
- TypeScript `interface`/`type`/`class`/`enum`: the header only (`export interface Item`, `export type Kind`, `export class Store extends Base`).

A signature is cut to 160 characters with `text::clip`. A long parameter list loses its tail, not its name.

### 3.6 Ranking (PR 2)

**Graph.** One node per mapped or referencing file. For each reference to `name` in file A and each file B that defines `name` (B ≠ A), add the edge A → B, labelled `name`, with this weight:

```
weight = mul(name) × sqrt(count of `name` references in A)
mul(name) = 1
          × 10   if name is in Seeds.names
          × 10   if name is long and specific: ≥ 8 characters and snake_case, camelCase or PascalCase with ≥ 2 parts
          × 0.1  if name starts with `_`
          × 0.1  if more than 5 files define name (generic: `new`, `render`, `build` across modules)
```

These are Aider's constants. They're kept here as named constants in `rank.rs`, for tuning against section 5's numbers.

**Seeds.** `Seeds` comes from the same `Item` that "Where to start" uses:
- `files`: `StartFiles::files` that are source files, meaning they have a known `Lang`.
- `names`: the identifiers in:
  - the item's title and `remaining`
  - the backticked words in its spec files (the same text `named_code_paths` already reads)

  An identifier is a backticked word, or any `snake_case` or `PascalCase` word of 4 or more characters. Words containing `/` or `.` are paths, and they already go to `files`.

**PageRank.**
- Damping is 0.85.
- Iterate until the L1 change between iterations is under 1e-6, or for at most 50 iterations.
- Mass from a node with no out-edges is redistributed by the personalisation vector.
- Personalisation:
  - When `Seeds.files` is non-empty, each seed file gets 1/|seeds| and every other file 0.
  - With no seed files, it's uniform. This is plain PageRank, which ranks the most-used files highest.
- Everything is in `f64` and deterministic. Nodes are iterated in sorted path order and ties are broken by path, so the same checkout and seeds always give the same map. Tests depend on this.

**Definitions.** Each file's rank is shared out over its out-edges in proportion to their weights, and each edge's share is credited to the definition it points at, `(B, name)`. If B defines `name` more than once (overloads, trait impls), each copy gets an equal part. A definition's score is the sum of what it received, plus `rank(B) × 1e-3`, so that a definition nothing references still sorts within its file by file rank rather than being lost. Definitions are sorted by score, highest first, with ties broken by file path and then line.

**Seed files first.** A definition in a seed file gets ×2 on its score. "Where to start" already pointed the session at that file, and the map's job there is to say what's in it so the session can read only the part it needs.

**PR 1 has no ranking.** Definitions are ordered by file path, then line, and the budget cut takes them in that order. That's enough to test extraction and rendering, and PR 1 sends nothing to a prompt.

### 3.7 Budget, layout and time limit

**Budget cut.** Use binary search to find the largest N for which the first N ranked definitions, grouped and rendered as below, fit in `maxTokens × 4` characters. The header's characters count too. If even one definition doesn't fit, the map is empty.

**Layout.** Definitions are grouped by file. Files are ordered by their best definition's rank, and definitions within a file by line. A method follows its type when both are shown. Two-space indent, no bullets, no code fences. Fences would cost tokens, and the session reads this as text:

```
## Repo map

What this project defines and where, most relevant to this item first: each file's public functions and types, with the first line of its doc comment. Check it before you search; Read the file for anything more.

src/implementer/start_files.rs
  pub const MAX_FILES: usize — At most this many files across the three parts of "Where to start" (section 6.2).
  pub struct StartFiles
  pub async fn build(git_bin: &Path, root: &Path, base_ref: &str, item: &Item) -> StartFiles — Builds [`StartFiles`] for `item`, reading its specs from `root` …
  pub fn render(start: &StartFiles) -> String — The "Where to start" prompt section's text (section 6.2), empty when there's nothing to list.
src/text.rs
  pub fn collapse_whitespace(s: &str) -> String
  pub fn clip(s: &str, n: usize) -> String
```

- A description is preceded by ` — ` (a spaced em dash). When there is no doc comment, nothing follows the signature.
- A file with more than 12 definitions in the cut shows its top 12, then `  … and N more`. This stops one big file from crowding everything else out.
- `render` of an empty map is the empty string, so the prompt gets no heading and no hint text.

**Time limit.** `build` runs the whole scan in `spawn_blocking` under `tokio::time::timeout(REPO_MAP_TIMEOUT)` (5 seconds). On timeout, the map is empty and the session starts without one. The scan isn't stopped mid-file, but the blocking task checks an `AtomicBool` between files and stops early.

Target performance, measured in PR 1 and written into its description:
- this repo (about 400 Rust files) in under 500 ms on the reference Windows machine in a debug build
- Retaskable.com (about 1,500 TS/JS files) in under 2 seconds

Parsing is single-threaded per map. Lanes already run in parallel, so splitting a map across threads would only compete with the other lanes.

### 3.8 Failure never blocks a session

The map is a help, never a gate. When git fails, a grammar doesn't load, the time limit is hit, every file is filtered out or a panic is caught (`catch_unwind` around the blocking task), the result is an empty map and the session starts as it would have without one. This matches how `start_files::build` treats git errors ("a git error leaves the list shorter and the session still starts").

Every build logs one `repo_map` event in the project's event log ([agent-telemetry.md](agent-telemetry.md) section 3). Its fields:
- `item`
- `files` (scanned)
- `defs` (shown)
- `chars`
- `ms`
- `truncated`
- `outcome`: `ok`, `empty`, `timeout` or `error`
- `error`: the first 200 characters of the reason, only when `outcome` is `error`

A map that's empty because the project has no supported files is `empty`, not an error. The event is logged whether the flag is on or off (section 2.8).

### 3.9 In the prompt (PR 2)

**Which sessions get it.** These sessions, only when they're fresh, and only with the `repo-map` flag On:
- **The implementer** (`src/orchestrator/session.rs`, where `where_to_start` is built today).
- **The implementer on a piece** (`src/orchestrator/piece_runs.rs`'s `build`).

Other sessions:
- **The planner, the drafter and the scouts** don't get it in this spec. They're the next candidates (the drafter's 26-of-40-turns figure is the strongest case), once section 5 shows the implementer gains.
- **The reviewer** sees a diff and doesn't need a map.
- **Codex, Cursor and pi sessions** of the same roles get the same text. The map is plain prompt text and has nothing Claude-specific in it.

**Where it goes.** `start_files::render`'s text is followed by `repo_map::render`'s text, joined by a blank line. Together they're the `where_to_start` string that `implementer/prompt.rs` already appends to the end of the prompt. No new template placeholder is needed, and the prompt files don't change. The order is "Where to start", then "Changed since its specs last did", then "Repo map": first the files to read, then what changed in them, then everything else.

The caller builds both in parallel (`tokio::join!`), since one runs git and the other parses. `ClaudeStarter` and the piece runner need the project's `Flags`, which the driver already holds (`self.flags`, as `asks.rs` uses for `ask-claude`), passed in when the starter is made.

**Wording.** The map's own header line (section 3.7) is the only instruction it carries. "Where to start"'s line is changed when the map is present, from:

> Read the ones you need together, several Read calls in one message, before searching.

to:

> Read the ones you need together, several Read calls in one message. The repo map below says what else is defined where: check it before you search.

The wording for when there's no map stays exactly as it is today, so with the flag Off the prompt is byte-for-byte unchanged. A test (section 4) asserts both versions.

**Size check.** The prompt with the map stays inside the session's start budget: about 1,500 tokens on top of a prompt that's already 3,000–6,000.

### 3.10 Config

The `repoMap` key in a project's settings (`config::project`, serde camelCase, as `codeGates` is):

| Key | Default | Meaning |
|---|---|---|
| `repoMap.maxTokens` | `1500` | The budget (section 2.7). `0` turns the map off for this project, even with the flag On. |

The time limit, file caps and skip lists are constants in code, not settings. They protect the computer, not taste, and nobody should have to tune them. The default comes from one place, a `Default` impl that the serde default calls, so no `#[serde(default = ...)]` literal can bake in a value that a later fallback never sees.

### 3.11 The `retaskable repo-map` command (PR 1)

`retaskable repo-map [--root <path>] [--tokens <n>] [--seed <file>]... [--name <ident>]...` prints the map for a checkout (default: the current directory), followed by one stats line: files scanned, definitions shown, characters, milliseconds.

The command only reads the checkout and the terminal. It doesn't touch the store, the cloud link or the HTTP port. It's for checking what a session will see, for tuning, and for the PR descriptions' timing numbers. It lives in `src/main.rs`'s command list next to `start-report`, and in `src/repo_map.rs`'s `print_cli`, so that `main.rs` doesn't grow (it's already over its soft limit).

## 4. Tests

All in `src/repo_map/tests.rs` (split by area if it nears 500 lines), against fixtures in `src/repo_map/fixtures/`, with no git needed except in the discovery tests, which run `git init` in a temp folder as `start_files.rs`'s tests do.

**Queries and languages (PR 1)**
- Every query file compiles against its grammar. This is a test, so a grammar bump that breaks a query fails CI rather than giving an empty map at run time.
- `Lang` from an extension, for every extension in section 3.2 and an unknown one.

**Extraction (PR 1), one fixture per language with known contents**
- Rust:
  - `pub fn`, `pub(crate) struct`, a `pub` method in an `impl` shown as `Type::method`, a `pub trait` with its method signatures, `pub const`, `macro_rules!` and `pub mod` are found.
  - A private `fn`, and a `pub fn` inside `#[cfg(test)] mod tests`, are not.
- TypeScript:
  - `export function`, `export class` with a public method, `export interface`, `export type`, `export const f = () =>` and `export default function` are found.
  - A non-exported function, a `private` method and a `#private` method are not.
- TSX: a component (`export function Panel(props: Props): JSX.Element`) parses with the TSX grammar, not the TypeScript one.
- JavaScript: ESM exports and `module.exports.x =` are found, and a file with JSX parses.
- References:
  - A call, a type use, a `use` path's last segment and an import specifier are captured.
  - A two-letter name and a stoplisted name are dropped.
- A file with a syntax error keeps the definitions outside the broken node.

**Doc comments and signatures (PR 1)**
- Doc comments:
  - `///` above `#[derive(...)]` above a struct is that struct's doc.
  - A `//` (non-doc) comment is not.
  - A doc comment followed by a blank line before the item is not.
  - A JSDoc block's `@param` lines are dropped.
- First sentence:
  - It stops at `. ` but not inside backticks, and not at `e.g.`.
  - It's cut at 100 characters.
- Signatures:
  - A multi-line Rust `fn` signature collapses to one line ending before `{`.
  - A `where` clause is kept.
  - A struct shows its header only, and a const has no value.
  - A 300-character signature is cut to 160.

**Discovery (PR 1)**
- Only tracked files are listed: an untracked `.rs` file is not.
- Each skip rule drops its case:
  - a `vendor/` path
  - an `@generated` header
  - `.min.js`
  - a file over 256 KB
  - non-UTF-8 bytes
- A test file adds references but no definitions.
- On Windows, a nested path (`src/a/b.rs`) is read correctly and printed with forward slashes.
- A git failure (a folder that isn't a repo) gives an empty map with outcome `error`, and no panic.

**Rendering and budget (PR 1, by path order; PR 2, by rank)**
- The layout matches section 3.7 exactly for a small fixture. This is a golden string.
- No doc comment means no ` — `.
- A file with 15 definitions shows 12 and `… and 3 more`.
- At every budget from 50 to 3,000 tokens in steps, the output is at most `budget × 4` characters, and it's the largest prefix that fits. Adding one more definition would exceed the budget.
- An empty map renders as `""`.

**Ranking (PR 2), on a fixture graph of six files with known references**
- With no seeds, the file everything calls ranks first.
- Seeding a leaf file moves the leaf and its direct dependencies above the hub.
- A name in `Seeds.names` lifts the file defining it.
- A name defined in six files weighs less than a unique one.
- The same input gives byte-identical output 10 times running (determinism).
- `Seeds` from an `Item`:
  - The identifiers come from the title, from `remaining` and from a spec's backticks.
  - Paths go to `files`, not `names`.

**Prompt (PR 2, in `src/implementer/prompt.rs` and `src/orchestrator/` tests)**
- Flag Off:
  - The rendered prompt is byte-identical to today's. Assert this against a fixture prompt, not only `contains`.
  - A `repo_map` event is still logged.
- Flag On with a non-empty map:
  - The prompt ends with "Where to start", then "Repo map".
  - "Where to start"'s line is the with-map wording.
- Flag On with an empty map: the prompt is the flag-Off prompt.
- A resumed session gets no map whatever the flag.
- `repoMap.maxTokens: 0` gives no map with the flag On.
- A time-limit hit (a test hook that makes the scan sleep) gives no map and a `timeout` event, and the session still starts.

**Telemetry (PR 2)**
- `start_stats` reads whether the session's first prompt contained `## Repo map`.
- `start_stats_text` prints the with-map and without-map medians separately.

## 5. Measuring it

The question is whether sessions with a map reach their first edit in fewer turns, and for less cost, than sessions without one.

- **The data is already kept.** `retaskable start-report` ([agent-telemetry.md](agent-telemetry.md) section 5) reads each fresh implementer session's transcript and reports, before its first edit:
  - turns
  - calls
  - share of the session's cost
  - the share of single-call turns
- **The new split (PR 2).** `start_stats` also reads whether the session's first user message contains `## Repo map`. The report prints one line of medians for each group: with map, then without. The transcript already holds the prompt, so no new event or field is needed to tell the groups apart.
- **The comparison.** Turn `repo-map` On for the project's agent computers and leave it Off for one of them, or alternate it by week. Then compare at least 20 sessions in each group.

**Success** is when sessions with a map have:
- a median of no more than 12 turns before the first edit, against 19 without one, or 40% fewer turns if the baseline has since moved
- a lower median share of the session's cost before the first edit
- no rise in implementer sessions ending `failed` or `needs_human`, from the agent report's phases

**Then:**
- **If it succeeds:** remove the flag, keeping the map, and write a follow-up for the drafter and the planner.
- **If it doesn't:** look at what the sessions searched for before their first edit. A search for a name that the map didn't show points at the ranking or the budget. A search for a name it did show points at the wording. Tune once, measure again, and remove the map if the second round shows no gain either.

## 6. Pieces

### 6.1 PR 1: the map

Contains:
- Cargo.toml's four tree-sitter crates
- `src/repo_map.rs`, `discover.rs`, `lang.rs`, `extract.rs`, `doc.rs` and `render.rs`, with ordering by path
- the query files and fixtures
- `repoMap.maxTokens` in project config
- `retaskable repo-map`
- the `repo_map` event type, logged by nothing yet; the command doesn't log
- section 4's tests marked PR 1

No prompt changes.

The PR description records:
- the release binary's size growth on Windows, Linux and macOS
- the command's time on this repo and on Retaskable.com

Done when the command prints a sensible map for both repos, and `cargo fmt`, `cargo clippy --all-targets -- -D warnings` and `cargo test` pass on all three platforms in CI.

### 6.2 PR 2: ranking and the prompt

Contains:
- `rank.rs`
- `Seeds` from the item
- the ranked budget cut
- building the map in `orchestrator/session.rs` and `orchestrator/piece_runs.rs` alongside `start_files::build`, logging the `repo_map` event every time
- the flag-gated prompt join and the with-map wording in `start_files::render`
- `Flags` passed to the starters
- `start_stats`'s with/without split
- section 4's tests marked PR 2
- [agent/06.2-implementer.md](agent/06.2-implementer.md) and [agent-telemetry.md](agent-telemetry.md) section 5, each given a sentence pointing here

Done when, with the flag On, a fresh implementer session's transcript shows the map after "Where to start". With it Off, the prompt is unchanged, and the report splits its medians.

## 7. Acceptance

- [ ] `retaskable repo-map` in this repo lists `start_files::build` and `render` with their signatures and doc lines, and lists no private function and nothing from a test module.
- [ ] `retaskable repo-map` in Retaskable.com lists exported TS functions with their JSDoc first lines, and nothing from `node_modules` or a `.test.ts`.
- [ ] The map is within budget at the default, and the same checkout and seeds give byte-identical output on Windows, Linux and macOS (paths in forward slashes on all three).
- [ ] It builds in under 2 seconds on Retaskable.com. A forced timeout or git failure gives a session with no map, never a failed start.
- [ ] With `repo-map` Off, every prompt is byte-identical to before. With it On, fresh implementer and piece sessions get the map and resumed ones don't.
- [ ] Nothing is written to disk except the `repo_map` event line. No new process is spawned beyond the existing `git ls-files`, and nothing leaves the computer.
- [ ] `start-report` shows the with-map and without-map medians, and section 5's comparison has been run and its result recorded here.

## 8. Open questions

- **Tokens per character.** 4 is the usual estimate for English and code, but Rust signatures are punctuation-heavy, which pushes the true count up. If section 5 shows the prompt growing more than expected, measure with the API's token counter on a sample and adjust the divisor. Don't add a tokenizer.
- **Crate-path references.** In Rust, `use crate::text::clip` says exactly which file defines `clip`, but matching by name alone treats every `clip` the same. Resolving `crate::` paths to files would make the graph sharper for Rust. Do it only if the ranking test cases show cross-module name collisions hurting.
- **The drafter and the planner.** They read specs more than code. Their map might be better seeded from the spec's named code than from "Where to start". Decide when section 5 says the implementer map works.
