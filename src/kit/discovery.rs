//! Finds a project's UI kit (docs/specs/project-kits.md, TS `kitDiscovery.ts`). READ-ONLY and deterministic: it lists folders and
//! reads a few small text files in the project's checkout, and never runs anything of the project's (no dev server, no Storybook, no
//! import of its components, no install, no git). Plain code detects the signals and chooses the tier; for a `catalogue` an optional
//! read-only session may tidy the found names into the closed kit ([`discover`]), and any invalid or missing answer falls back to
//! `wireframe`.
use std::collections::{BTreeSet, HashSet, VecDeque};
use std::fs;
use std::io::Read;
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{Genre, KIT_MAX_PAGES, KIT_MAX_PIECES, Kit, Page, Piece, Tier, Tokens, cap_kit, read_kit, wireframe_kit};

/// How far the read goes: depth, entries listed, files read (times three, for the small ones) and bytes read from each.
const DEPTH: usize = 4;
const ENTRIES: usize = 4000;
const READS: usize = 80 * 3;
const BYTES: usize = 64 * 1024;

const SKIP_DIRS: [&str; 30] = [
    "node_modules",
    ".git",
    ".hg",
    ".svn",
    "dist",
    "build",
    "out",
    ".next",
    ".nuxt",
    ".expo",
    "Pods",
    ".venv",
    "venv",
    "__pycache__",
    "target",
    "Library",
    "Temp",
    "Obj",
    "obj",
    "bin",
    ".gradle",
    ".idea",
    ".vscode",
    "coverage",
    ".cache",
    ".turbo",
    "DerivedData",
    ".import",
    "vendor",
    ".build",
];

/// What was found, before a tier is chosen.
#[derive(Debug, Clone)]
pub struct Found {
    /// Retaskable's own project.
    pub library: bool,
    pub genre: Genre,
    /// Plain words for what was seen ("react-native", "storybook", "godot"...), for the fingerprint.
    pub signals: Vec<String>,
    pub pieces: Vec<Piece>,
    pub tokens: Tokens,
    pub pages: Vec<Page>,
    pub fingerprint: String,
}

struct Entry {
    rel: String,
    dir: bool,
    size: u64,
}

fn re(p: &str) -> Regex {
    Regex::new(p).unwrap()
}

/// Lists the checkout, bounded; never follows a link out of it.
fn walk(root: &Path) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut queue = VecDeque::from([(root.to_path_buf(), String::new(), 0usize)]);
    while let Some((abs, rel, depth)) = queue.pop_front() {
        if out.len() >= ENTRIES {
            break;
        }
        let Ok(read) = fs::read_dir(&abs) else { continue };
        let mut names: Vec<_> = read.flatten().collect();
        names.sort_by_key(|e| e.file_name());
        for d in names {
            if out.len() >= ENTRIES {
                break;
            }
            let name = d.file_name().to_string_lossy().into_owned();
            let r = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            let Ok(kind) = d.file_type() else { continue };
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                out.push(Entry {
                    rel: r.clone(),
                    dir: true,
                    size: 0,
                });
                if depth < DEPTH && !SKIP_DIRS.contains(&name.as_str()) {
                    queue.push_back((d.path(), r, depth + 1));
                }
            } else if kind.is_file() {
                out.push(Entry {
                    rel: r,
                    dir: false,
                    size: d.metadata().map_or(0, |m| m.len()),
                });
            }
        }
    }
    out
}

/// Reads small text files, counting them: past the limit it answers nothing.
struct Reader<'a> {
    root: &'a Path,
    reads: usize,
}

impl Reader<'_> {
    /// The first `bytes` bytes of a small text file, or "".
    fn head(&mut self, rel: &str, bytes: usize) -> String {
        if self.reads >= READS {
            return String::new();
        }
        self.reads += 1;
        let Ok(file) = fs::File::open(self.root.join(rel)) else {
            return String::new();
        };
        let mut buf = Vec::new();
        if file.take(bytes as u64).read_to_end(&mut buf).is_err() {
            return String::new();
        }
        String::from_utf8_lossy(&buf).into_owned()
    }
}

fn base(rel: &str) -> &str {
    rel.rsplit('/').next().unwrap_or(rel)
}

fn stem(rel: &str) -> String {
    let b = base(rel);
    match b.rfind('.') {
        Some(at) if at > 0 => b[..at].to_string(),
        _ => b.to_string(),
    }
}

/// "UserProfileScreen" and "user-profile" as "User profile": a name a person reads.
pub fn name_of(raw: &str) -> String {
    static SUFFIX: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\.(stories|story|component|screen|page|view)$"));
    static KIND_WORD: LazyLock<Regex> = LazyLock::new(|| re(r"(Screen|Page|View)$"));
    static CAMEL: LazyLock<Regex> = LazyLock::new(|| re(r"([a-z0-9])([A-Z])"));
    static SEPS: LazyLock<Regex> = LazyLock::new(|| re(r"[-_.]+"));
    let s = SUFFIX.replace(raw, "");
    let s = KIND_WORD.replace(&s, "");
    let s = CAMEL.replace_all(&s, "$1 $2");
    let s = SEPS.replace_all(&s, " ");
    let words = s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    let mut chars = words.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).take(40).collect(),
        None => String::new(),
    }
}

/// A piece or page id: camelCase letters and digits from a name (all lower case when `lower`).
fn id_of(name: &str, taken: &mut HashSet<String>, pattern: &Regex, lower: bool) -> String {
    let lowered = name.to_lowercase();
    let parts = lowered
        .split(|c: char| !c.is_ascii_lowercase() && !c.is_ascii_digit())
        .filter(|p| !p.is_empty());
    let mut id: String = parts
        .enumerate()
        .map(|(i, p)| {
            if i > 0 && !lower {
                let mut cs = p.chars();
                cs.next()
                    .map(|f| f.to_uppercase().chain(cs).collect::<String>())
                    .unwrap_or_default()
            } else {
                p.to_string()
            }
        })
        .collect();
    if id.is_empty() {
        id = "piece".into();
    }
    if !id.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
        id = format!("p{id}");
    }
    id.truncate(30);
    let first = id.clone();
    let mut n = 2;
    while taken.contains(&id) || !pattern.is_match(&id) {
        id = format!("{first}{n}");
        n += 1;
    }
    taken.insert(id.clone());
    id
}

static KIND_RULES: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        (r"(?i)button|btn|cta", "button"),
        (
            r"(?i)input|textfield|text field|textarea|select|dropdown|search|combobox|autocomplete|field|form",
            "input",
        ),
        (r"(?i)modal|dialog|popup|popover|alert|confirm", "dialog"),
        (r"(?i)sheet|drawer|panel", "sheet"),
        (r"(?i)table|grid|datagrid|spreadsheet", "table"),
        (r"(?i)tab ?bar|bottom ?(nav|bar)|tabs?$", "tabBar"),
        (r"(?i)header|navbar|nav ?bar|app ?bar|toolbar|topbar|top bar", "header"),
        (r"(?i)nav|menu|breadcrumb|pagination|stepper", "nav"),
        (r"(?i)list|item|row|feed|timeline", "list"),
        (r"(?i)avatar|image|img|icon|logo|thumbnail|photo|gallery|video", "image"),
        (r"(?i)chart|graph|canvas|map|board", "canvas"),
        (r"(?i)badge|tag|pill|label|status", "badge"),
        (r"(?i)chip|filter", "chip"),
        (r"(?i)switch|toggle|checkbox|radio|slider", "toggle"),
        (r"(?i)heading|title|headline", "heading"),
        (r"(?i)text|typography|paragraph|caption", "text"),
    ]
    .into_iter()
    .map(|(p, k)| (re(p), k))
    .collect()
});

/// The closed kind of a piece from its name (a plain table; unknown names are a card).
pub fn kind_of(name: &str) -> &'static str {
    KIND_RULES
        .iter()
        .find(|(r, _)| r.is_match(name))
        .map_or("card", |(_, k)| k)
}

static NOT_SCREENS: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)\b(probes?|screenshots?|screen shots?|shots?|benche?s?|tests?|debug|gallery|sandbox|demo|examples?|templates?|scratch|tmp|audit|compare|preview|mock|harness|tools?|bots?|capture|bake|bakes|generate|generator|render|export|import)\b",
    )
});
static SIDECAR: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)\s(text|parts|styles?|data|types?|utils?|helpers?|hooks?|logic)$"));
static NOT_PIECES: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)^(index|test|spec|utils?|helpers?|types?|constants?|styles?|hooks?|context|provider|stories|mock|fixtures?|layout|root|app|main|config)$",
    )
});
static GAME_DEPS: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"^(phaser|pixi\.js|@pixi/.*|three|babylonjs|@babylonjs/.*|kaboom|excalibur|melonjs|playcanvas|matter-js|@react-three/fiber|@dimforge/rapier.*|littlejsengine)$",
    )
});
static GRID_DEPS: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"^(ag-grid-.*|@ag-grid-.*|handsontable|@handsontable/.*|react-data-grid|@tanstack/(react-)?table|luckysheet|@glideapps/glide-data-grid|jspreadsheet.*|x-data-spreadsheet|univer|@univerjs/.*)$",
    )
});
static PY_GAME: LazyLock<Regex> = LazyLock::new(|| re(r"^(pygame|arcade|pyglet|panda3d|kivy)$"));
const UI_DEPS: [&str; 14] = [
    "react-native",
    "expo",
    "react",
    "vue",
    "svelte",
    "next",
    "nuxt",
    "solid-js",
    "@angular/core",
    "tailwindcss",
    "electron",
    "@storybook/react",
    "@storybook/vue3",
    "@storybook/react-native",
];

/// The dependency names of a package.json (never their versions: the fingerprint must not move on an upgrade).
fn deps_of(text: &str) -> Vec<String> {
    let Ok(j) = serde_json::from_str::<Value>(text) else {
        return Vec::new();
    };
    ["dependencies", "devDependencies", "peerDependencies"]
        .iter()
        .filter_map(|k| j.get(k)?.as_object())
        .flat_map(|o| o.keys().cloned())
        .collect()
}

fn norm(c: &str) -> String {
    let c = c.to_lowercase();
    if c.len() == 4 {
        let b: Vec<char> = c.chars().collect();
        format!("#{0}{0}{1}{1}{2}{2}", b[1], b[2], b[3])
    } else {
        c
    }
}

/// The numbers after the first word of `line` that matches `word`, each of one to three digits standing alone ("16", "8px").
fn numbers_after(line: &str, word: &Regex) -> Vec<u32> {
    let Some(m) = word.find(line) else { return Vec::new() };
    let chars: Vec<char> = line[m.start()..].chars().collect();
    let word_char = |c: char| c.is_alphanumeric() || c == '_';
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if !chars[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && chars[i].is_ascii_digit() {
            i += 1;
        }
        let before_ok = start == 0 || !(word_char(chars[start - 1]) || matches!(chars[start - 1], '#' | '.' | '-'));
        let mut end = i;
        if chars.get(end) == Some(&'p') && chars.get(end + 1) == Some(&'x') {
            end += 2;
        }
        let after_ok = chars.get(end).is_none_or(|c| !word_char(*c) && *c != '%');
        if before_ok && after_ok && i - start <= 3 {
            out.push(chars[start..i].iter().collect::<String>().parse().unwrap_or(0));
        }
    }
    out
}

/// Colours, spacing, radius and fonts from the text of token files (plain patterns; nothing is evaluated).
pub fn tokens_from(texts: &[String]) -> Tokens {
    static COLOR: LazyLock<Regex> = LazyLock::new(|| {
        re(r#"(?:^|[\s,{(])['"]?(?:--)?([A-Za-z][\w-]{0,30})['"]?\s*[:=]\s*['"]?(#[0-9a-fA-F]{6}|#[0-9a-fA-F]{3})\b"#)
    });
    static SPACING: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)spac(?:e|ing)|gap|padding|margin"));
    static RADIUS: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)radius|rounded"));
    static FONT: LazyLock<Regex> =
        LazyLock::new(|| re(r#"(?i)font-?family\s*[:=]\s*['"`]?([A-Za-z][A-Za-z0-9 -]{1,38})"#));
    static KEY_SEP: LazyLock<Regex> = LazyLock::new(|| re(r"[-_]+([a-z0-9])"));
    let mut colors = std::collections::BTreeMap::new();
    let mut spacing = BTreeSet::new();
    let mut radius = BTreeSet::new();
    let mut fonts: Vec<String> = Vec::new();
    for text in texts {
        for line in text.lines().take(1500) {
            if let Some(c) = COLOR.captures(line)
                && colors.len() < 12
            {
                let raw = KEY_SEP
                    .replace_all(&c[1], |m: &regex::Captures| m[1].to_uppercase())
                    .into_owned();
                let mut key: String = raw.chars().take(23).collect();
                if let Some(f) = key.chars().next().filter(|f| f.is_ascii_uppercase()) {
                    key.replace_range(0..1, &f.to_ascii_lowercase().to_string());
                }
                if key.chars().next().is_some_and(|f| f.is_ascii_lowercase())
                    && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                    && !colors.contains_key(&key)
                {
                    colors.insert(key, norm(&c[2]));
                }
            }
            for n in numbers_after(line, &SPACING) {
                if spacing.len() < 8 {
                    spacing.insert(n.min(128));
                }
            }
            for n in numbers_after(line, &RADIUS) {
                if radius.len() < 6 {
                    radius.insert(n.min(64));
                }
            }
            if let Some(f) = FONT.captures(line) {
                let name = f[1].trim().to_string();
                if fonts.len() < 3 && !fonts.contains(&name) {
                    fonts.push(name);
                }
            }
        }
    }
    Tokens {
        colors: (!colors.is_empty()).then_some(colors),
        spacing: (!spacing.is_empty()).then(|| spacing.into_iter().map(f64::from).collect()),
        radius: (!radius.is_empty()).then(|| radius.into_iter().map(f64::from).collect()),
        fonts: (!fonts.is_empty()).then_some(fonts),
    }
}

/// The screens, routes or scenes the file names show: names only, never their content.
fn pages_from(entries: &[Entry], file_routes: bool) -> Vec<Page> {
    static PAGE_ID: LazyLock<Regex> = LazyLock::new(|| re(r"^[a-z][a-z0-9]{0,30}$"));
    static CODE: LazyLock<Regex> = LazyLock::new(|| re(r"\.(tsx|jsx|vue|svelte|ts|js)$"));
    static NOT_CODE: LazyLock<Regex> = LazyLock::new(|| re(r"\.(test|spec|stories|d)\."));
    static SCREEN_DIR: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^(screens?|pages?|routes?|views?)$"));
    static HTML_SKIP: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^(index|404|offline)$"));
    static OUT_DIR: LazyLock<Regex> =
        LazyLock::new(|| re(r"(?i)^(dist|build|coverage|output|out|reports?|docs?|site|public|static)$"));
    static NOT_ROUTE: LazyLock<Regex> =
        LazyLock::new(|| re(r"^(layout|_layout|\+.*|route|loading|error|not-found|template|global-error)$"));
    let mut taken = HashSet::new();
    let mut pages: Vec<Page> = Vec::new();
    let mut add = |name: String, kind: &str| {
        if pages.len() >= KIT_MAX_PAGES
            || name.is_empty()
            || NOT_PIECES.is_match(&name)
            || NOT_SCREENS.is_match(&name)
            || SIDECAR.is_match(&name)
            || name.starts_with(['_', '+', '[', '('])
            || pages.iter().any(|p| p.name == name && p.kind == kind)
        {
            return;
        }
        let id = id_of(&name, &mut taken, &PAGE_ID, true);
        pages.push(Page {
            id,
            name,
            kind: kind.into(),
        });
    };
    for e in entries.iter().filter(|e| !e.dir) {
        let parts: Vec<&str> = e.rel.split('/').collect();
        let file = parts[parts.len() - 1];
        let dirs = &parts[..parts.len() - 1];
        let ext = file.rsplit('.').next().unwrap_or("");
        if matches!(ext, "tscn" | "unity" | "umap") && file.contains('.') {
            add(name_of(&stem(file)), "scene");
        } else if matches!(ext, "html" | "htm")
            && dirs.len() <= 1
            && !HTML_SKIP.is_match(&stem(file))
            && !dirs.iter().any(|d| OUT_DIR.is_match(d))
        {
            add(name_of(&stem(file)), "screen");
        } else if CODE.is_match(file) && !NOT_CODE.is_match(file) {
            let at = dirs.iter().position(|d| SCREEN_DIR.is_match(d));
            let app_at = dirs.iter().position(|d| *d == "app");
            let is_app = file_routes && app_at.is_some_and(|a| a <= 1 && dirs[..a].iter().all(|d| *d == "src"));
            if let Some(at) = at.filter(|at| dirs.len() - at <= 3) {
                let name = if stem(file) == "index" {
                    if dirs[dirs.len() - 1] == dirs[at] {
                        "Home".to_string()
                    } else {
                        name_of(dirs[dirs.len() - 1])
                    }
                } else {
                    name_of(&stem(file))
                };
                add(name, "screen");
            } else if let (true, Some(a)) = (is_app, app_at)
                && dirs.len() <= a + 3
            {
                let route = stem(file);
                if route == "page" || route == "index" {
                    add(
                        if dirs.len() > a + 1 {
                            name_of(dirs[dirs.len() - 1])
                        } else {
                            "Home".into()
                        },
                        "route",
                    );
                } else if !NOT_ROUTE.is_match(&route) {
                    add(name_of(&route), "route");
                }
            }
        }
    }
    pages
}

fn sha(inputs: &[String]) -> String {
    let digest = Sha256::digest(inputs.join("\n").as_bytes());
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// Reads what the project has: signals, pieces, tokens, pages, and a fingerprint of the discovery inputs. Never runs anything of
/// the project's.
pub fn scan(root: &Path) -> Found {
    let entries = walk(root);
    let files: Vec<&Entry> = entries.iter().filter(|e| !e.dir).collect();
    let has = |rel: &str| entries.iter().any(|e| e.rel == rel);
    let mut reader = Reader { root, reads: 0 };
    let mut signals: Vec<String> = Vec::new();
    let mut deps: BTreeSet<String> = BTreeSet::new();

    // Retaskable's own project.
    let library = has("app/src/ui/whiteboardDesign.ts") && has("shared/designLayout.ts");

    // package.json at the root and one or two levels down (monorepos), requirements for Python.
    for p in files
        .iter()
        .filter(|e| base(&e.rel) == "package.json" && e.rel.split('/').count() <= 3)
        .take(8)
    {
        deps.extend(deps_of(&reader.head(&p.rel, BYTES)));
    }
    static REQ_FILE: LazyLock<Regex> = LazyLock::new(|| re(r"^(requirements.*\.txt|pyproject\.toml|Pipfile)$"));
    static PY_DEP: LazyLock<Regex> = LazyLock::new(|| {
        re(r#"(?im)^\s*["']?(pygame|arcade|pyglet|panda3d|kivy|tkinter|streamlit|gradio|flask|django|fastapi)\b"#)
    });
    for r in files
        .iter()
        .filter(|e| REQ_FILE.is_match(base(&e.rel)) && e.rel.split('/').count() <= 2)
        .take(4)
    {
        let text = reader.head(&r.rel, 16 * 1024);
        for m in PY_DEP.captures_iter(&text) {
            deps.insert(m[1].to_lowercase());
        }
    }
    for d in UI_DEPS {
        if deps.contains(d) {
            signals.push(d.into());
        }
    }
    for d in &deps {
        if GAME_DEPS.is_match(d) || GRID_DEPS.is_match(d) || PY_GAME.is_match(d) {
            signals.push(d.clone());
        }
    }
    let storybook = entries.iter().any(|e| e.dir && base(&e.rel) == ".storybook");
    if storybook {
        signals.push("storybook".into());
    }
    static TAILWIND: LazyLock<Regex> = LazyLock::new(|| re(r"^tailwind\.config\.(js|cjs|mjs|ts)$"));
    let tailwind: Vec<&&Entry> = files
        .iter()
        .filter(|e| TAILWIND.is_match(base(&e.rel)) && e.rel.split('/').count() <= 3)
        .collect();
    if !tailwind.is_empty() {
        signals.push("tailwind".into());
    }
    let godot = files.iter().any(|e| base(&e.rel) == "project.godot");
    let unity = files
        .iter()
        .any(|e| e.rel.ends_with("ProjectSettings/ProjectVersion.txt"));
    let unreal = files.iter().any(|e| e.rel.ends_with(".uproject"));
    for (on, name) in [(godot, "godot"), (unity, "unity"), (unreal, "unreal")] {
        if on {
            signals.push(name.into());
        }
    }
    let game_deps = deps
        .iter()
        .any(|d| GAME_DEPS.is_match(d) || PY_GAME.is_match(d) || d == "panda3d");
    let grid_deps = deps.iter().any(|d| GRID_DEPS.is_match(d));
    let genre = if godot || unity || unreal || game_deps {
        Genre::Game
    } else if grid_deps {
        Genre::Data
    } else {
        Genre::App
    };

    // Pieces: stories first (their titles), then component folders' file names.
    static PIECE_ID: LazyLock<Regex> = LazyLock::new(|| re(r"^[A-Za-z][A-Za-z0-9-]{0,40}$"));
    let mut taken = HashSet::new();
    let mut seen = HashSet::new();
    let mut pieces: Vec<Piece> = Vec::new();
    let mut add_piece = |pieces: &mut Vec<Piece>, raw: &str, options: Vec<String>| {
        let name = name_of(raw);
        if name.is_empty()
            || NOT_PIECES.is_match(&name)
            || seen.contains(&name.to_lowercase())
            || pieces.len() >= KIT_MAX_PIECES
        {
            return;
        }
        seen.insert(name.to_lowercase());
        let id = id_of(&name, &mut taken, &PIECE_ID, false);
        let kind = kind_of(&name).to_string();
        pieces.push(Piece {
            id,
            name,
            kind,
            options: (!options.is_empty()).then_some(options),
        });
    };
    static STORY: LazyLock<Regex> = LazyLock::new(|| re(r"\.stories\.(tsx?|jsx?|mdx|svelte|vue|mjs)$"));
    static STORY_TITLE: LazyLock<Regex> = LazyLock::new(|| re(r#"title:\s*['"`]([^'"`]+)['"`]"#));
    static ARG_TYPES: LazyLock<Regex> = LazyLock::new(|| re(r"(?s)argTypes:\s*\{(.{0,700})"));
    static ARG_NAME: LazyLock<Regex> = LazyLock::new(|| re(r"(?m)^\s{2,8}(\w+):"));
    static ARG_SKIP: LazyLock<Regex> = LazyLock::new(|| re(r"^(control|options|description|table|action|name|type)$"));
    for s in files
        .iter()
        .filter(|e| STORY.is_match(&e.rel) && !e.rel.contains("node_modules"))
        .take(60)
    {
        let text = reader.head(&s.rel, 4096);
        let title = STORY_TITLE.captures(&text).map(|c| c[1].to_string());
        let opts: Vec<String> = ARG_TYPES
            .captures(&text)
            .map(|c| c[1].to_string())
            .iter()
            .flat_map(|t| ARG_NAME.captures_iter(t).map(|m| m[1].to_string()).collect::<Vec<_>>())
            .filter(|o| o.len() <= 24 && !ARG_SKIP.is_match(o))
            .take(8)
            .collect();
        let name = title.unwrap_or_else(|| stem(&s.rel));
        add_piece(&mut pieces, name.rsplit('/').next().unwrap_or(&name), opts);
    }
    static COMP_DIR: LazyLock<Regex> =
        LazyLock::new(|| re(r"(?i)^(components?|ui|design-system|primitives|atoms|molecules)$"));
    static COMP_FILE: LazyLock<Regex> = LazyLock::new(|| re(r"\.(tsx|jsx|vue|svelte)$"));
    static JSX_FILE: LazyLock<Regex> = LazyLock::new(|| re(r"\.(tsx|jsx)$"));
    static TEST_FILE: LazyLock<Regex> = LazyLock::new(|| re(r"\.(test|spec|stories)\."));
    let mut comp_files: Vec<String> = Vec::new();
    for dir in entries
        .iter()
        .filter(|e| e.dir && COMP_DIR.is_match(base(&e.rel)) && !e.rel.contains("/node_modules/"))
        .take(12)
    {
        let depth = dir.rel.split('/').count();
        for e in entries
            .iter()
            .filter(|e| e.rel.starts_with(&format!("{}/", dir.rel)) && e.rel.split('/').count() - depth <= 2)
        {
            let n = if e.dir {
                base(&e.rel).to_string()
            } else if COMP_FILE.is_match(&e.rel) && !TEST_FILE.is_match(&e.rel) {
                stem(&e.rel)
            } else {
                String::new()
            };
            if !n.is_empty() && n.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                add_piece(&mut pieces, &n, Vec::new());
            } else if !e.dir && JSX_FILE.is_match(&e.rel) && !TEST_FILE.is_match(&e.rel) && comp_files.len() < 30 {
                comp_files.push(e.rel.clone());
            }
        }
    }
    // A library of lower-case files (buttons.tsx, panels.tsx) names its pieces by what it exports: `export function Button`.
    static EXPORT: LazyLock<Regex> =
        LazyLock::new(|| re(r"(?m)^export\s+(?:default\s+)?(?:function|const|class)\s+([A-Z][A-Za-z0-9]{2,30})\b"));
    static NOT_EXPORTED: LazyLock<Regex> =
        LazyLock::new(|| re(r"(Props|Provider|Context|Type|Options|Config|State|Value|Item)$"));
    for rel in &comp_files {
        let text = reader.head(rel, 12 * 1024);
        for m in EXPORT.captures_iter(&text) {
            if !NOT_EXPORTED.is_match(&m[1]) {
                add_piece(&mut pieces, &m[1], Vec::new());
            }
        }
    }

    // Tokens: the first few token and theme files, and the tailwind config.
    static TOKEN_FILE: LazyLock<Regex> = LazyLock::new(|| {
        re(r"(?i)^(tokens?|theme|colou?rs?|palette|design-tokens|variables)\.(ts|tsx|js|mjs|cjs|json|css|scss|less)$")
    });
    let token_files: Vec<&&Entry> = files
        .iter()
        .filter(|e| TOKEN_FILE.is_match(base(&e.rel)) && !e.rel.contains("node_modules") && e.size < 200_000)
        .take(3)
        .collect();
    let token_rels: Vec<String> = token_files
        .iter()
        .map(|e| e.rel.clone())
        .chain(tailwind.iter().take(1).map(|e| e.rel.clone()))
        .collect();
    let token_texts: Vec<String> = token_rels.iter().map(|r| reader.head(r, BYTES)).collect();
    if !token_files.is_empty() {
        signals.push("token files".into());
    }

    static ROUTER: LazyLock<Regex> =
        LazyLock::new(|| re(r"^(expo-router|next|nuxt|@remix-run/.*|@sveltejs/kit|gatsby)$"));
    let pages = pages_from(&entries, deps.iter().any(|d| ROUTER.is_match(d)));
    let mut inputs: Vec<String> = deps
        .iter()
        .filter(|d| UI_DEPS.contains(&d.as_str()) || GAME_DEPS.is_match(d) || GRID_DEPS.is_match(d))
        .map(|d| format!("dep:{d}"))
        .chain(signals.iter().map(|s| format!("sig:{s}")))
        .chain(pieces.iter().map(|p| format!("piece:{}:{}", p.name, p.kind)))
        .chain(pages.iter().map(|p| format!("page:{}:{}", p.name, p.kind)))
        .chain(token_rels.iter().zip(&token_texts).map(|(r, t)| {
            format!(
                "token:{r}:{}",
                sha(std::slice::from_ref(t)).chars().take(8).collect::<String>()
            )
        }))
        .collect();
    if library {
        inputs.push("library".into());
    }
    inputs.sort();
    let mut seen_signals = HashSet::new();
    signals.retain(|s| seen_signals.insert(s.clone()));
    Found {
        library,
        genre,
        signals,
        pieces,
        tokens: tokens_from(&token_texts),
        pages,
        fingerprint: sha(&inputs),
    }
}

/// The plain-code choice (no session): `library` for Retaskable's own project, `catalogue` when the project's own pieces were found
/// (at least 3, or any under a Storybook), else the `wireframe` kit for its genre.
pub fn choose(found: &Found) -> Kit {
    let tokens = (!found.tokens.is_empty()).then(|| found.tokens.clone());
    let pages = (!found.pages.is_empty()).then(|| found.pages.clone());
    let fingerprint = Some(found.fingerprint.clone());
    if found.library {
        return Kit {
            tier: Tier::Library,
            genre: None,
            fingerprint,
            pieces: Vec::new(),
            tokens: None,
            pages: None,
        };
    }
    let catalogued =
        found.pieces.len() >= 3 || (found.signals.iter().any(|s| s == "storybook") && !found.pieces.is_empty());
    if catalogued
        && let Some(kit) = cap_kit(Kit {
            tier: Tier::Catalogue,
            genre: Some(found.genre),
            fingerprint: fingerprint.clone(),
            pieces: found.pieces.clone(),
            tokens: tokens.clone(),
            pages: pages.clone(),
        })
    {
        return kit;
    }
    wireframe_kit(found.genre, tokens, pages, fingerprint)
}

/// What the optional session reads: the found names and token lines, as plain text (never file content beyond that).
pub fn digest_of(found: &Found) -> String {
    let signals = if found.signals.is_empty() {
        "none".to_string()
    } else {
        found.signals.join(", ")
    };
    let pieces = if found.pieces.is_empty() {
        "(none)".to_string()
    } else {
        found
            .pieces
            .iter()
            .map(|p| match p.options.as_ref().filter(|o| !o.is_empty()) {
                Some(o) => format!("- {} (options: {})", p.name, o.join(", ")),
                None => format!("- {}", p.name),
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let colors = found.tokens.colors.as_ref().map_or(String::new(), |c| {
        c.iter().map(|(k, v)| format!("{k} {v}")).collect::<Vec<_>>().join(", ")
    });
    let text = format!(
        "Signals: {signals}\n\nPieces found:\n{pieces}\n\nColours: {}",
        if colors.is_empty() { "none".into() } else { colors }
    );
    text.chars().take(6000).collect()
}

/// How the kit was come by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum How {
    Library,
    Code,
    Session,
    Fallback,
    Wireframe,
}

/// The kit for a project's files: the code's choice, and for a catalogue the session's validated tidy of it (`summarise`: the digest
/// in, the structured answer out). An invalid or missing answer is the wireframe kit with the tokens and pages the code found.
/// Without a session the code's own catalogue stands.
pub async fn discover<F, Fut>(found: &Found, summarise: Option<F>) -> (Kit, How)
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = Option<Value>>,
{
    let chosen = choose(found);
    match chosen.tier {
        Tier::Library => return (chosen, How::Library),
        Tier::Wireframe => return (chosen, How::Wireframe),
        Tier::Catalogue => {}
    }
    let Some(summarise) = summarise else {
        return (chosen, How::Code);
    };
    if let Some(answer) = summarise(digest_of(found)).await {
        let mut candidate = serde_json::to_value(&chosen).unwrap_or_else(|_| json!({}));
        candidate["pieces"] = answer.get("pieces").cloned().unwrap_or(Value::Null);
        if let Some(kit) = read_kit(&candidate).and_then(cap_kit) {
            return (kit, How::Session);
        }
    }
    (
        wireframe_kit(
            found.genre,
            chosen.tokens,
            chosen.pages,
            Some(found.fingerprint.clone()),
        ),
        How::Fallback,
    )
}

#[cfg(test)]
mod tests;
