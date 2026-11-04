# Code Index — Spec

Status: v0.2, proposed 2026-11-04. Builds on [agent/06.2-implementer.md](agent/06.2-implementer.md) ("Where to start"), [engines.md](engines.md) section 3.4 (each session's MCP servers) and [agent-telemetry.md](agent-telemetry.md) section 5 (`start-report`). This is a **Rust-only** feature: the TypeScript agent is being retired, so there's no `agent/` counterpart, and Retaskable.com's specs don't carry it. This file is its source of truth, unlike the rest of this folder ([README.md](README.md)).

Theme: Implementer sessions
Kind: new

## 1. What it is

The **code index** is a lookup of every function, type and constant in a project. For each one it records where it is (file and line), its signature, and what it does (the first line of its doc comment). It also records which files use each name.

Sessions query it whenever they want, as often as they want, through tools: "where is `build_args` defined", "what in this project deals with release flags", "what's in `src/lanes.rs`", "what calls `render`". Each answer is a few lines, not a file read and not a page of grep output.

It keeps itself current:
- Every query first checks which files changed since the last one, including the session's own edits a minute ago and files it just created, and re-reads only those.
- What it learned is saved, so the next query, the next session and the other lanes start from it rather than from nothing.

A fresh session's prompt also gets a short **map**: the index's most relevant entries for the item, about 1,500 tokens. The session starts knowing roughly what exists and where, and asks the index for the rest.

**Why.** Sessions spend their first turns looking for where things live. On 2026-10-20, even with "Where to start", implementers took a median of 19 turns of 54, and 36% of the session's cost, before their first edit (`start-report`, 20 sessions), 92% of those turns a single search or read. On 2026-10-30, drafters spent 26 of 40 turns before their first write. Every turn resends the conversation; one lookup that returns three lines replaces a grep that returns forty and the reads after it.

**What it is not.**
- **Nothing to install.** CLAUDE.md's "no DB" means nothing heavy a person must install or run. The parsers compile into the agent's binary, and the saved index is one cache file in the repo's `.git` folder, never committed, synced or sent. Deleting it loses nothing.
- **Not embeddings.** "What it does" questions are answered by matching words against names, doc comments and paths (section 3.6). Section 2.1 says why vectors were left out.
- **Not a replacement for Read and Grep.** It says where to look. Grep is still for text that isn't a name (an error message, a config key).

## 2. Decisions

### 2.1 [Decided] Tree-sitter, not embeddings, a language server or ctags

**Chosen:** parse each source file with [tree-sitter](https://tree-sitter.github.io/). This finds its definitions (with signatures and doc comments) and the names it uses. Tree-sitter grammars compile into the binary, need no build of the project, no network and no install on the computer, and they parse a half-written file without falling over. Ranking the map by how files use each other (section 3.7) is the approach Aider's repo map popularised.

**Rejected:**
- **Embeddings (Cursor-style RAG).** These need an embedding model on every computer (the server must never see code) and re-embedding on every change, and they answer "code that looks like this sentence" rather than "the thing called X". Word matching over names and doc comments (section 3.6) covers "what does X" without them. Vectors can be added later if it falls short (section 8).
- **A language server or SCIP indexer.** Precise, but each language needs its own server installed and warmed up on every computer (rust-analyzer takes minutes cold).
- **ctags.** Another binary to install, with no reference data.

### 2.2 [Decided] Kept up to date by checking on every query, not by watching files

Before answering, a query lists the checkout's files and compares each file's size and modified time against the saved index (section 3.4). A file that changed is re-read and re-parsed, and a file that's gone is dropped. A file the session created is picked up, even before it's committed.

On a warm index this takes tens of milliseconds. There's no watcher process, no file-system events (which differ by platform and miss git's own changes) and no daemon to clean up, and an answer can't be stale.

### 2.3 [Decided] Saved in the repo's git folder, shared by all its checkouts

The index is saved to `<git common dir>/retaskable/code-index.json`. `git rev-parse --git-common-dir` gives the main `.git` folder, which every lane's worktree of the same repo shares. This means:
- One project has one index, shared by every lane, every session and the agent itself, with no project id needed to find it.
- It's never committed, since nothing under `.git` is.
- It's removed along with the repo.
- It works the same in a person's own checkout, so `retaskable code` is useful by hand too.

Entries are keyed by content hash (section 3.3), so lanes on different branches share every file they have in common.

### 2.4 [Decided] Sessions use it through tools: MCP where the engine has it, the command line everywhere

The same five queries (section 3.6) are offered two ways, both served by the agent's own binary:
- **`retaskable code <query>`**, a command. Any session can run it, on any engine.
- **`retaskable code mcp`**, a small MCP server over stdio. It offers the queries as tools, so Claude and Codex sessions get them as first-class tools rather than shell commands (section 3.9).

MCP is a thin layer over the same queries. No `rmcp` crate: the server speaks the four JSON-RPC messages it needs with `serde_json`, in under 200 lines.

### 2.5 [Decided] Languages: Rust, TypeScript, TSX and JavaScript first

These are the languages of this repo and Retaskable.com. Each language is a grammar crate and two query files (section 3.2), and a file in any other language is not indexed. Python and Go are next.

### 2.6 [Decided] What it records

**Definitions:**
- Rust: every `fn` (including methods in `impl`s, named `Type::method`), `struct`, `enum`, `trait` (and its method signatures), `type`, `const`, `static`, `macro_rules!` and `mod`.
- TypeScript and JavaScript: every function, class (and its methods), interface, type, enum, top-level `const`/`let` holding a function or a class, and CommonJS `module.exports.x =` assignment.

Private items are included, because "where is that helper" is a real question. Each definition records whether it's **public**:
- Rust: any visibility modifier.
- TypeScript and JavaScript: exported, and for methods, neither `private`/`protected` nor `#`-named.

The prompt map shows public definitions only (section 3.7). Queries show both, public first.

**Doc lines.** A definition's "what it does" is the first sentence of its doc comment, cut at 120 characters:
- Rust: `///`, or `//!` for a module.
- TypeScript and JavaScript: the `/** ... */` block directly above.

A definition with no doc comment has no doc line. Nothing is generated or guessed.

**Uses.** Each file's list of the names it uses, with how many times. Test code is indexed like any other code, but marked as test. Queries list test definitions last and the map leaves them out. Their uses still count, since a test calling something is evidence that it matters.

### 2.7 [Decided] Behind the `repo-map` release flag, measured by `start-report`

The flag (`src/flags.rs`) controls whether sessions are given the tools, the prompt line and the map. Off means sessions are unchanged. Section 5 compares sessions with and without, and then the flag is removed one way or the other.

### 2.8 [Decided] Three pull requests

1. **The index and the command** (section 6.1). Nothing changes for sessions.
2. **The tools in sessions** (section 6.2): the MCP server, giving it to sessions, and the prompt line, behind the flag.
3. **The map in the prompt** (section 6.3): ranking, the map after "Where to start", and the `start-report` split.

## 3. Behavior

### 3.1 Where the code lives

A new module, `src/code_index/`. Every file stays under 500 lines (code gates: soft 500, hard 1,000).

| File | What it holds |
|---|---|
| `src/code_index.rs` | Module root: `pub struct Index`, `pub fn open(root: &Path) -> Index`, `pub fn refresh(&mut self) -> Refresh` (section 3.4), and the query functions' re-exports. |
| `src/code_index/discover.rs` | Which files to index (section 3.4). |
| `src/code_index/lang.rs` | `enum Lang { Rust, TypeScript, Tsx, JavaScript }` from an extension, and each grammar and compiled query, loaded once (`OnceLock`). |
| `src/code_index/extract.rs` | One file's text to its `FileEntry` (section 3.2). |
| `src/code_index/doc.rs` | Doc lines and signatures for a definition node. |
| `src/code_index/store.rs` | Loading and saving `code-index.json` (section 3.3). |
| `src/code_index/query.rs` | `find`, `search`, `outline`, `uses` (section 3.6). |
| `src/code_index/rank.rs` | The file graph and personalised PageRank (section 3.7). PR 3. |
| `src/code_index/map.rs` | The prompt map and its token budget (section 3.7). PR 3. |
| `src/code_index/mcp.rs` | The stdio MCP server (section 3.8). PR 2. |
| `src/code_index/cli.rs` | `retaskable code …` argument handling and printing, so `main.rs` (already over its soft limit) gains one match arm only. |
| `src/code_index/queries/{rust,typescript,javascript}-{defs,uses}.scm` | Tree-sitter queries, `include_str!`'d. |
| `src/code_index/tests/` and `src/code_index/fixtures/` | Tests (section 4). The fixtures are small source files, one folder per language. |

**Dependencies:**
- `tree-sitter`
- `tree-sitter-rust`
- `tree-sitter-typescript` (both `LANGUAGE_TYPESCRIPT` and `LANGUAGE_TSX`)
- `tree-sitter-javascript`

All four must be on the same ABI, and they're pinned in Cargo.lock. The grammars are C, compiled by `cc`: with MSVC on Windows (already required by the Rust install) and the system compiler on Linux and macOS. CI and the release builds must still pass on all three platforms. PR 1 records the release binary's growth for each one.

**Types** (all `serde`):
```rust
pub struct Def {
    pub name: String,          // `build`, `StartFiles`, `Type::method`
    pub kind: DefKind,         // Fn, Method, Struct, Enum, Trait, Type, Const, Static, Macro, Mod, Class, Interface
    pub line: u32,             // 1-based
    pub signature: String,     // section 3.2
    pub doc: Option<String>,   // section 2.6
    pub public: bool,
}
pub struct FileEntry {
    pub lang: Lang,
    pub test: bool,
    pub defs: Vec<Def>,
    pub uses: Vec<(String, u32)>, // name, count; sorted by name
}
```

### 3.2 Reading one file

Each language has two queries, run with `tree_sitter::QueryCursor`:

**`*-defs.scm`** captures `@def.<kind>` on the definition node and `@name` on its name. It also captures:
- for Rust: `@vis` on a `visibility_modifier`, and `@impl.type` on an enclosing `impl`'s type, which gives the `Type::method` name
- for TypeScript and JavaScript: `@export` on an enclosing `export_statement`

**`*-uses.scm`** captures names that are used, not defined:
- Rust: `identifier`, `type_identifier` and `field_identifier` in call, method-call, field, path, struct-expression, type and macro positions, plus the last segment of a `use` path.
- TypeScript and JavaScript: `identifier`, `type_identifier` and `property_identifier` in call, `new`, member, type-annotation, JSX-element and import-specifier positions.

These are this repo's own query files, not the grammars' `tags.scm`, so a grammar update can't quietly change results.

**Rules:**
- **Test code.** A Rust definition inside a `mod` with a `#[cfg(test)]` attribute is marked as test. So is everything in a test file: `code_gates::is_test`, a `tests.rs`, a path under `tests/`, or a name like `*.test.*` or `*.spec.*`.
- **Syntax errors.** A node with `has_error()` drops only the captures inside it. The rest of a half-written file still counts.
- **Short and common names.** Uses shorter than 3 characters are dropped, and so are uses in a per-language stoplist (`new`, `default`, `from`, `into`, `clone`, `len`, `map`, `push`, `get`, `set`, `then`, `length`, `toString`, `Ok`, `Err`, `Some`, `None`, …). These link everything to everything.
- **Doc comments.** Walk the definition's previous siblings:
  - Skip attributes (`#[derive]`, `#[cfg]`, …) and decorators.
  - Collect contiguous doc comments.
  - Stop at any other node, or at a blank line.

  Then strip the comment markers and drop JSDoc `@tag` lines. Collapse whitespace with `text::collapse_whitespace`. Take the first sentence: up to `. ` or the end, not inside backticks, and not at `e.g.`/`i.e.`. Finally, `text::clip` it to 120 characters.
- **Signatures.** A signature is the source text from the definition's start to its body, with whitespace collapsed and cut to 160 characters:
  - A function stops before its block.
  - A `struct`, `enum`, `trait`, `class` or `interface` is its header only.
  - A `const` stops before `=`.
  - An exported arrow function shows as `export const name = (params) => Ret`.

### 3.3 The saved index

**File:** `<git common dir>/retaskable/code-index.json`. The path comes from `git rev-parse --git-common-dir`, run once per process in the checkout, and is joined with `Path::join`. It contains:

```json
{
  "version": 1,
  "grammars": "rust=0.24.0;typescript=0.23.2;javascript=0.23.1;queries=<sha256 of the .scm files>",
  "entries": { "<sha256 of file content>": { "lang": "rust", "test": false, "defs": [...], "uses": [...], "seen": "2026-11-04" } },
  "files":   { "<checkout root>": { "<path>": { "size": 1234, "mtime": 1730700000123, "hash": "<sha256>" } } }
}
```

- **`entries`** is keyed by content hash, so identical files on two branches share one entry.
- **`files`** is kept separately for each checkout (the lane's root, as an absolute path), so each lane's quick size-and-time check is its own.
- **`grammars`** names the versions that built the entries. When it differs from the running binary's (a grammar update, or an edited query), the whole file is discarded and rebuilt. A `version` mismatch does the same.

**Saving:**
- Write to `code-index.json.<pid>.tmp` in the same folder, then rename it over the old file. Rename is atomic on Windows (`MoveFileEx` with replace, which `std::fs::rename` uses), Linux and macOS.
- Before writing, re-read the file and merge: the union of `entries`, and this checkout's `files` replacing its old ones. A rare lost save only costs a re-parse.
- A save that fails is logged at debug level and otherwise ignored, and the query still answers from memory. Two cases where this happens:
  - a sandbox that won't let a session write under `.git` (Codex's `workspace-write` keeps `.git` read-only)
  - a full disk

**Pruning on save:**
- An entry no checkout's `files` points at, and not `seen` for 14 days, is dropped.
- A `files` block for a checkout root that no longer exists (a removed lane) is dropped.
- If the file is still over 50 MB, the oldest-`seen` entries go first until it's under.

A file that isn't readable JSON is treated as empty and overwritten on the next save.

### 3.4 Refreshing before every query

`Index::refresh`:
1. **List files.** Run `git ls-files -z --cached --others --exclude-standard` in the checkout, using `lanes::git_output_in_worktree`. This gives tracked files plus new files not yet added, and leaves out anything `.gitignore` excludes (`target/`, `node_modules/`, build output).
2. **Filter.** Keep files with a known `Lang`:
   - `.rs`
   - `.ts`, `.mts`, `.cts`, `.d.ts`
   - `.tsx`
   - `.js`, `.mjs`, `.cjs`, `.jsx`

   Then drop:
   - paths with a segment named `vendor`, `third_party`, `node_modules`, `dist`, `build`, `out`, `target`, `generated` or `__generated__`
   - files over 256 KB
   - `.min.js` files
   - files whose first 512 bytes contain `@generated` or `DO NOT EDIT`
   - files with a line over 1,000 characters in their first 4 KB
   - files that aren't UTF-8

   At most 5,000 files are kept. Past that, `truncated` is set in the answer.
3. **Check each file.** `stat` it (size, modified time in milliseconds):
   - If both match this checkout's `files` record and that hash has an entry, the file is unchanged.
   - Otherwise, read it and hash it (`sha2`). If an entry with that hash exists (from another lane or branch), use it and refresh its `seen`. If not, parse the file.
4. **Drop missing files.** Files in the `files` record but no longer listed are dropped from it.
5. **Save** if anything changed (section 3.3).

The result `Refresh { files, parsed, reused, removed, truncated, ms }` is printed with `--stats` and logged with each prompt map (section 3.10).

**Speed targets**, measured in PR 1 on the reference Windows machine (release build):
- a cold index of Retaskable.com (about 1,500 files) in under 3 seconds
- a warm query with nothing changed in under 150 ms
- a warm query after editing one file in under 200 ms

Parsing runs on one thread. Lanes already run side by side, so a single query shouldn't compete with them.

**Paths.**
- Inside the index and in every answer, paths are repo-relative with forward slashes, as git prints them, on every platform. These match "Where to start" and work with every engine's file tools.
- They're joined to the root one component at a time, never pasted together as strings.
- The `files` key, an absolute root, is canonicalised (`std::fs::canonicalize`, with `\\?\` stripped on Windows) so the same lane always gets the same key.

### 3.5 When the agent refreshes it

The index is already warm when a session starts. The agent refreshes it in the lane right after `lanes::prepare`, alongside `start_files::build`, for every fresh implementer and piece session with the flag On. It does this on a blocking thread (`spawn_blocking`) with a 20-second time limit.

Only the first session on a new computer pays the cold cost. If the limit is hit, the session starts anyway and its first query finishes the job.

### 3.6 The queries

Every query refreshes first (section 3.4), then answers in plain text. Each hit is one line:

```
src/engines/claude.rs:88  pub fn build_args(spec: &SessionSpec) -> Vec<String> — The `claude -p` arguments for a session.
```

The format is `path:line`, two spaces, the signature, then ` — ` and the doc line if there is one. Public definitions come before private ones, and test ones come last. Each answer is capped at `limit` lines (default 20), with `… N more` when cut.

| Query | Command | Answers |
|---|---|---|
| **find** | `retaskable code find <name> [--limit N]` | Definitions whose name is `<name>`, or ends in `::<name>`. This is case-sensitive. If none match exactly, the closest names are shown: case-insensitive, then a prefix match, then names within edit distance 2. |
| **search** | `retaskable code search <words…> [--limit N]` | Definitions ranked by how well their words match (BM25). A definition's words are its name split at `_`, `::` and case changes (`buildArgs` → `build args`), the words of its doc line, and its file path's segments. Name words count 3× and path words 0.5×. Words are compared lowercased, with simple English suffixes stripped (`-s`, `-es`, `-ing`, `-ed`), so `flags` matches `flag`.  |
| **outline** | `retaskable code outline <path>` | Every definition in the file, in line order, nested methods indented under their type. |
| **uses** | `retaskable code uses <name> [--limit N]` | The files that use `<name>`, with how many times, most first, excluding the files that define it. Matching is by name, not resolved by type, so a common name lists more than its real callers. The answer says so in one line when `<name>` is defined in more than one file. |
| **map** | `retaskable code map [--tokens N] [--seed <path>]… [--name <ident>]…` | The ranked overview (section 3.7). |

All of them take `--root <path>` (default: the current folder's checkout) and `--stats`. The command exits 0 with an answer, 1 when there's no checkout or no git, and 2 for bad arguments.

### 3.7 The map (PR 3)

**Graph.** One node per indexed file. For each name file A uses and each other file B that defines it publicly, add an edge A → B with this weight:

```
weight = mul(name) × sqrt(uses of name in A)
mul(name) = 1
  × 10   if name is one the item mentions (Seeds.names)
  × 10   if name is long and specific: ≥ 8 characters, snake/camel/Pascal case, ≥ 2 parts
  × 0.1  if name starts with `_`
  × 0.1  if more than 5 files define name
```

These are Aider's constants, kept as named constants in `rank.rs` for tuning.

**Seeds** come from the item that "Where to start" uses:
- `files`: "Where to start"'s files that have a `Lang`.
- `names`: identifiers in the item's title, its `remaining`, and the backticked words of its spec files. An identifier is a backticked word, or a `snake_case`/`PascalCase` word of 4 or more characters. Words containing `/` or `.` are paths, and go to `files`.

**PageRank.**
- Damping 0.85.
- Iterate until the L1 change is under 1e-6, or for at most 50 rounds.
- Rank lost at files with no outgoing edges is handed back out by the personalisation.
- Personalisation is 1/|seeds| on each seed file, or uniform with no seeds.
- `f64` throughout, with nodes taken in sorted path order and ties broken by path, so the same index and seeds always give the same map.

**Definitions.**
- Each file's rank is shared over its outgoing edges by weight, and each share is credited to the definition `(B, name)`, split evenly when B defines `name` more than once.
- Each definition also gets `rank(B) × 1e-3`, so an unused definition still sorts by its file's rank.
- Definitions in seed files get ×2.
- Only public, non-test definitions are shown.

**Budget.**
- Binary-search for the largest number of top definitions whose rendered text fits in `codeIndex.mapTokens × 4` characters, header included. Tokens are estimated at 4 characters each.
- Within that cut, definitions are grouped by file. Files are ordered by their best definition, and definitions within a file by line.
- A file shows at most 12 definitions, then `  … and N more`.
- An empty map renders as an empty string.

```
## Code index

These are the functions and types most relevant to this item; ask the code tools for anything else.

src/implementer/start_files.rs
  pub const MAX_FILES: usize — At most this many files across the three parts of "Where to start" (section 6.2).
  pub async fn build(git_bin: &Path, root: &Path, base_ref: &str, item: &Item) -> StartFiles — Builds [`StartFiles`] for `item`…
  pub fn render(start: &StartFiles) -> String — The "Where to start" prompt section's text…
src/text.rs
  pub fn collapse_whitespace(s: &str) -> String
```

### 3.8 The MCP server (PR 2)

`retaskable code mcp --root <lane>` serves MCP over stdio (newline-delimited JSON-RPC 2.0, protocol version `2025-06-18`):
- It answers `initialize` with its name `retaskable-code` and the `tools` capability.
- It ignores `notifications/initialized`.
- It answers `tools/list` with five tools.
- It answers `tools/call` by running the query and returning one `text` content block. A query error is a tool result with `isError: true`, never a protocol error.
- It exits when stdin closes.

The index is opened once and kept in memory for the life of the server. Each call refreshes it (section 3.4) and saves it if anything changed.

| Tool | Input | Description shown to the model |
|---|---|---|
| `code_find` | `name` (string), `limit` (int, optional) | "Where a function, type or constant is defined: its file, line, signature and what it does. Use before Grep when you know the name." |
| `code_search` | `query` (string), `limit` | "Find code by what it does, in plain words, matched against names, doc comments and paths. Use before Grep when you don't know the name." |
| `code_outline` | `path` (string) | "Every definition in a file, with line numbers, so you can Read only the part you need." |
| `code_uses` | `name`, `limit` | "Which files use a name, by count. Matches by name, so a common name lists extra files." |
| `code_map` | `tokens` (int, optional) | "An overview of the project's most-used public functions and types." |

The server writes only the index file, runs only `git ls-files` and `git rev-parse`, opens no network connection and listens on no port.

### 3.9 Giving it to sessions (PR 2)

With the flag On, these sessions get the tools:
- fresh and resumed implementer sessions
- implementer-piece sessions
- chore sessions

A resumed session must get the same tools its transcript already used. The planner, the drafter and the scouts are the next candidates (section 8). The reviewer works from a diff and doesn't need the tools.

**The binary.** The command used is the agent's own executable (`std::env::current_exe()`), canonicalised, so a session can never pick up a different `retaskable` on its PATH.

**By engine:**
- **Claude.** `engines/claude.rs`'s `build_args` keeps `--strict-mcp-config`. Alongside the project's own `.mcp.json`, it passes a second `--mcp-config` file, written to the item's runs folder as `<session>.mcp.json`:
  ```json
  {"mcpServers":{"retaskable-code":{"command":"<exe>","args":["code","mcp","--root","<lane>"]}}}
  ```
  It also adds `mcp__retaskable-code__code_find`, `code_search`, `code_outline`, `code_uses` and `code_map` to `--allowedTools`, so auto mode never stops to ask about them. It's deleted when the session ends.
- **Codex.** The same server is added with `-c mcp_servers.retaskable-code.command="<exe>"` and `-c mcp_servers.retaskable-code.args=[...]`. Codex keeps its own sandbox, so the server may not be able to save under `.git`; it answers anyway (section 3.3), from the index warmed before the session (section 3.5).
- **Cursor and pi.** No MCP is added, because Cursor reads MCP servers from the project's own folder and that is the project's to configure, and pi has none. Their prompt names the command instead (below).

**Prompt line.** With the flag On, the implementer, piece and chore prompts get one more line in "Where to start", or as their own short section when there's no "Where to start". For Claude and Codex:

> To find where something is defined or what does a job, use the `code_find` and `code_search` tools before Grep; `code_outline` shows a file's definitions so you can Read only what you need.

For Cursor and pi:

> To find where something is defined or what does a job, run `"<exe>" code find <name>` or `"<exe>" code search <words>` before searching; `"<exe>" code outline <path>` shows a file's definitions so you can Read only what you need.

`<exe>` is the quoted absolute path, with backslashes on Windows. The engine runs it through its own shell, so the quoting is what that shell expects.

**Flag Off.** With the flag Off, none of this is added. The arguments and prompts are byte-for-byte what they are today.

### 3.10 The map in the prompt (PR 3)

With the flag On, a fresh implementer or piece session's prompt ends with "Where to start", then "Changed since its specs last did", then the **map** (section 3.7). The map text is joined onto the same `where_to_start` string that `implementer/prompt.rs` already appends, so no template changes are needed. The map is built from the index warmed in section 3.5. A resumed session gets no map: its transcript has one.

Each built map logs one `code_index` event in the project's event log ([agent-telemetry.md](agent-telemetry.md) section 3) with these fields:
- `item`
- `files`
- `parsed`
- `reused`
- `defs` (shown)
- `chars`
- `ms`
- `truncated`
- `outcome`: `ok`, `empty`, `timeout` or `error`
- `error`: the first 200 characters, only when `outcome` is `error`

### 3.11 Never in the way

The index helps; it never blocks anything:
- **When the agent warms it or builds the map,** git failing, the time limit, a grammar that won't load, or a caught panic (`catch_unwind` around the blocking work) all mean no warm index and no map. The session starts exactly as it would have.
- **When a session calls a tool,** the same failures return a short error text telling it to use Grep instead.
- **If the MCP server process dies,** the engine reports the tool as unavailable and the session carries on with its own tools.

None of these is a `failed` session or a `needs_human`.

### 3.12 Config

The `codeIndex` key in a project's settings (`config::project`, serde camelCase, as `codeGates` is):

| Key | Default | Meaning |
|---|---|---|
| `codeIndex.mapTokens` | `1500` | The prompt map's budget. `0` turns the map off for this project. |
| `codeIndex.tools` | `true` | `false` keeps the tools away from this project's sessions, even with the flag On. |

The default comes from one place, a `Default` impl that the serde default calls, so no `#[serde(default = …)]` literal can bake in a value that a later fallback never sees. File caps, time limits, skip lists and pruning are constants in code, not settings.

## 4. Tests

All tests are under `src/code_index/tests/`, split by area, each file under 500 lines. They run against fixtures. The tests that need git run `git init` in a temp folder, as `start_files.rs`'s tests do.

**Extraction (PR 1)**
- Every query file compiles against its grammar, so a grammar bump that breaks one fails CI.
- `Lang` is chosen correctly for every extension in section 3.4, and an unknown extension gets none.
- Rust:
  - `fn`, `pub(crate) struct`, `impl` methods as `Type::method`, `pub trait` with its methods, `const`, `macro_rules!` and `mod` are all found, with `public` right for both `pub` and private items.
  - Items in a `#[cfg(test)] mod` are marked as test.
- TypeScript: exported and non-exported functions, a class with public, `private` and `#private` methods, an interface, a type and an exported arrow `const`, each with the right `public`.
- TSX: a component file uses the TSX grammar.
- JavaScript: ESM exports, `module.exports.x =`, and JSX all parse.
- Uses:
  - A call, a type, a `use` path's last segment and an import specifier are captured, with counts.
  - Two-letter and stoplisted names are dropped.
- A file with a syntax error keeps the definitions outside the broken node.
- Doc lines:
  - `///` above `#[derive]` belongs to the struct.
  - A plain `//` comment is not a doc line.
  - A blank line breaks the link between comment and definition.
  - JSDoc `@param` lines are dropped.
  - The first sentence isn't cut inside backticks or at `e.g.`, and is clipped at 120 characters.
- Signatures:
  - A multi-line Rust signature becomes one line, `where` clause kept.
  - A struct shows its header only, and a const has no value.
  - A 300-character signature is clipped to 160.

**The saved index and refreshing (PR 1)**
- A cold refresh parses every file, and a second refresh parses none.
- Editing one file re-parses only it. Its new definition is found and its old one isn't.
- A new, untracked file is found. A `.gitignore`d file and a deleted file are not.
- Two checkouts of the same repo (two `git worktree add`s on different branches) share one `code-index.json`. Each parses only the files that differ from the other.
- A changed `grammars` string discards the file.
- A corrupt file is treated as empty and then overwritten.
- If the index folder is read-only, queries still answer.
- Pruning drops entries unseen for 14 days that no checkout points at, and a removed checkout's `files` block.
- Two refreshes saving at once (two threads) both leave a readable file holding entries from both, or at worst from one.
- Each skip rule drops its case (`vendor/`, `@generated`, `.min.js`, over 256 KB, non-UTF-8, a long first line).
- Paths:
  - On Windows, a nested path is read, and answers print it with forward slashes.
  - A lane root reached through a different spelling (case, `\\?\`) maps to the same `files` key.

**Queries (PR 1)**
- `find`:
  - An exact name, and a `Type::method` matched by `method`.
  - A miss falls back to case-insensitive, then prefix, then edit-distance matches.
  - Public results come before private ones, and test results come last.
- `search`: "release flag" ranks `flag_on` (documented "Whether a release flag is On") above a path-only match, and `flags` matches `flag`.
- `outline` lists a file's definitions in line order, with methods under their type.
- `uses` lists files by count without the defining file, with the "matched by name" note for a name defined twice.
- The output format matches section 3.6 exactly. This is checked against a golden string.
- `limit` and `… N more` work.
- Exit codes are 0, 1 and 2.

**MCP and sessions (PR 2)**
- An `initialize`, `tools/list`, `tools/call` exchange over in-memory pipes:
  - It returns the five tools with their input schemas.
  - A `code_find` call returns the same text as the command.
  - An unknown tool, or bad input, is an `isError` result, not a protocol error.
  - Closed stdin ends the server.
- Claude with the flag On: the second `--mcp-config` names the canonical executable and the lane, the five tools are in `--allowedTools`, `--strict-mcp-config` stays, and `--dangerously-skip-permissions` never appears.
- Codex with the flag On: the two `-c mcp_servers.retaskable-code` settings, and the sandbox still `read-only`/`workspace-write`.
- Cursor and pi prompts name the quoted executable path.
- Flag Off: every engine's arguments and every prompt are byte-identical to today's, checked against fixtures, not just `contains`.
- `codeIndex.tools: false` with the flag On gives no tools and no line.
- A resumed session gets the same tools as its fresh start did.

**The map (PR 3)**
- Ranking, on a six-file fixture: unseeded, the hub ranks first; seeding a leaf lifts it and its dependencies above the hub; a `Seeds.names` name lifts its file; a name defined in six files weighs less than a unique one; ten runs are byte-identical.
- Seeds from an `Item`: identifiers from the title, `remaining` and spec backticks, and paths go to `files`.
- Budget: from 50 to 3,000 tokens the map fits and is the largest cut that does, public non-test definitions only, at most 12 per file, and `""` when empty.
- The prompt: flag On puts the map last; flag Off, a resume or `mapTokens: 0` gives none; a forced timeout gives none, a `timeout` event, and a normal start.
- `start_stats`:
  - It reads whether the first prompt had `## Code index`.
  - It counts `code_*` tool calls and `retaskable code` shell calls before the first edit.
  - `start_stats_text` prints the with-index and without-index medians separately.

## 5. Measuring it

The question is whether sessions with the index reach their first edit in fewer turns and for less cost, and whether they use the tools at all.

- **The data is already kept.** `retaskable start-report` reads each fresh implementer session's transcript and reports, before its first edit:
  - turns
  - calls
  - share of the session's cost
  - single-call turns
- **Telling the groups apart (PR 3).** A session had the index if its first prompt contains `## Code index`, or the code-tools line from section 3.9. No new event is needed.
- **New in PR 3:**
  - The report prints medians for each group separately.
  - It counts index lookups: `code_*` tool calls, and `retaskable code` commands for Cursor and pi.
- **Running the comparison.** Turn `repo-map` On for all but one of a project's computers, or alternate by week. Then compare at least 20 sessions in each group.

**Success** is when sessions with the index have:
- a median of 12 or fewer turns before the first edit, against 19 without, or 40% fewer if the baseline has moved
- a lower median share of cost before the first edit
- the index tools in at least half of them, without nudging
- no rise in implementer sessions ending `failed` or `needs_human`

**If it works:** remove the flag, keeping the index, and write the follow-up for the drafter and the planner.

**If it doesn't:** read what sessions grepped for before their first edit. A name the index had but wasn't asked for points at the prompt line, one it lacked at extraction, and a badly ranked `code_search` at the word matching. Fix one and measure again.

## 6. Pieces

### 6.1 PR 1: the index and the command

Contains:
- the four tree-sitter crates
- `src/code_index.rs`, `discover.rs`, `lang.rs`, `extract.rs`, `doc.rs`, `store.rs`, `query.rs` and `cli.rs`
- the query files and fixtures
- `retaskable code find|search|outline|uses` with `--root`, `--limit` and `--stats`
- section 4's PR 1 tests

No session changes.

The PR description records the binary's growth on each platform and cold and warm timings on both repos.

Done when the command answers sensibly on both repos, a second run is warm, and `cargo fmt`, `cargo clippy --all-targets -- -D warnings` and `cargo test` pass in CI on all three platforms.

### 6.2 PR 2: the tools in sessions

Contains:
- `mcp.rs` and `retaskable code mcp`
- warming the index after `lanes::prepare` (section 3.5)
- Claude's and Codex's argument changes
- the prompt line for every engine
- `codeIndex.tools`
- `Flags` passed to the starters, as the driver already holds them for `ask-claude`
- section 4's PR 2 tests
- a sentence in [agent/06.2-implementer.md](agent/06.2-implementer.md) and [engines.md](engines.md) section 3.4, each pointing here

Done when, with the flag On, a real Claude session on staging calls `code_find` and its transcript shows the answer, and with the flag Off nothing differs.

### 6.3 PR 3: the map and the measurement

Contains:
- `rank.rs` and `map.rs`
- `Seeds` from the item
- `retaskable code map`
- the map after "Where to start"
- the `code_index` event
- `codeIndex.mapTokens`
- the `start_stats` split and lookup counts
- section 4's PR 3 tests
- a sentence in [agent-telemetry.md](agent-telemetry.md) section 5

Done when a fresh session's transcript shows the map, and `start-report` prints both groups.

## 7. Acceptance

- [ ] In this repo, `retaskable code find build_args` answers `src/engines/claude.rs:<line>` with its signature in under 150 ms warm, and `retaskable code search "release flag"` puts the flags client in the top five.
- [ ] An edited doc comment or a new file shows in the very next query. Deleting `code-index.json` loses nothing.
- [ ] Two lanes of one project share one index, and the second parses only what its branch changed.
- [ ] With `repo-map` On, Claude and Codex sessions have the tools without a permission prompt, and Cursor and pi are told the command. Off, every session is byte-identical to before.
- [ ] Codex stays in its sandbox, Claude in `--permission-mode auto`, no port is opened, nothing leaves the computer, and a broken index never fails a session.
- [ ] Section 5's comparison has been run and its result recorded here.

## 8. Open questions

- **Vectors on top.** If section 5 shows `code_search` missing what sessions meant ("the bit that decides when to give up"), add a small local embedding model over the same entries, stored in the same file, and blend its score with BM25. Only do this with the numbers in hand: it costs a model download on every computer.
- **Resolving Rust paths.** `use crate::text::clip` names the exact file that defines `clip`. Resolving `crate::`, `super::` and `self::` paths would make `uses` exact and the map's graph sharper for Rust. Do it if name collisions show up as wrong answers.
- **Other roles.** The drafter (26 of 40 turns before its first write) and the planner read specs more than code. They may want the tools without the map, or a map seeded from the spec's named code. Decide once section 5 is in.
- **Tokens per character.** 4 may run low on punctuation-heavy Rust. If prompts grow more than expected, measure a sample with the API's token counter and adjust the divisor.
