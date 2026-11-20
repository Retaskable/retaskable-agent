//! The settings page's builder `h()` (assets/settings-rt.js), run in node against a stand-in for the DOM that prints
//! a child the way a browser does when `append` is given something that is not a node or a string: an array becomes
//! its items' `toString()` joined, an element becomes `[object HTMLLIElement]`. The wizard once passed a `map()` result
//! as a child and showed "[object HTMLLIElement]" as text (setup-wizard.md section 3.8).
const SCRIPT: &str = r#"
const fs = require('node:fs');
class El {
  constructor(tag) { this.tag = tag; this.kids = []; this.className = ''; }
  append(...xs) { for (const x of xs) this.kids.push(x instanceof El ? x : String(x)); }
  toString() { return '[object HTML' + (this.tag === 'li' ? 'LI' : 'Unknown') + 'Element]'; }
  text() { return this.kids.map((k) => (k instanceof El ? k.text() : k)).join(''); }
}
global.window = {};
global.document = { createElement: (t) => new El(t), getElementById: () => null };
new (require('node:vm').Script)(fs.readFileSync(process.argv[1], 'utf8')).runInThisContext();
const h = window.RT.h;
const rows = ['a', 'b', 'c'].map((n) => h('li', '', h('span', 'wz-title', n)));
const list = h('ul', 'wz-list', rows);
const nested = h('ul', 'wz-list', [rows, [null, false, undefined]], 'tail');
const out = list.text() + '|' + nested.text();
if (out.includes('[object ')) { console.error('printed as text: ' + out); process.exit(1); }
if (out !== 'abc|abctail') { console.error('unexpected: ' + out); process.exit(1); }
if (list.kids.length !== 3 || !list.kids.every((k) => k instanceof El)) { console.error('children are not the elements'); process.exit(1); }
"#;

#[test]
fn a_list_of_elements_is_children_not_text() {
    let page = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/settings-rt.js");
    match crate::spawn::command("node").args(["-e", SCRIPT, "--", page]).output() {
        Ok(o) => assert!(
            o.status.success(),
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        ),
        Err(_) => eprintln!("no node here; skipped"),
    }
}

#[test]
fn no_page_script_joins_or_spreads_elements_into_text() {
    for file in [
        "settings-wizard.js",
        "settings-projects.js",
        "settings-programs.js",
        "settings-secrets.js",
    ] {
        let text = std::fs::read_to_string(format!("{}/assets/{file}", env!("CARGO_MANIFEST_DIR"))).unwrap();
        assert!(!text.contains("[object "), "{file} writes the placeholder itself");
        assert!(
            !text.contains(".join(\"\")") || !text.contains("h(\"li\""),
            "{file} joins li elements into a string"
        );
    }
}
