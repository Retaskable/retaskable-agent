//! A plan's live details while it is drafted (plan-live-details.md section 2, plan-stage.md section 3; TS
//! `shared/planProgressLines.ts`): what the drafting session has done so far, read in plain code from its transcript lines,
//! never asked of the model and never its words. "Reading the specs" from the first read of a spec, then "Writing <title>" once
//! per spec file it writes or edits, and one piece per spec file written, titled the same. No path or tool name ever shows.
use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Value, json};

use crate::text::clip;

/// The longest line or title, and the most lines or pieces kept.
pub const LINE_MAX: usize = 100;
pub const MAX: usize = 12;

static SPEC: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?:^|[\\/])docs[\\/]specs[\\/][^\\/]+\.md$").unwrap());
static HEADING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^#\s+(.+?)\s*$").unwrap());

/// One progress line, with the epoch seconds it was first seen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub at: i64,
    pub line: String,
}

/// What the lines of a transcript come to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Live {
    pub lines: Vec<Line>,
    /// The pieces' titles, in the order their files were first written.
    pub pieces: Vec<String>,
}

impl Live {
    /// The `progress` field of a draft's details: `[{ at, line }]`.
    pub fn progress(&self) -> Value {
        Value::Array(
            self.lines
                .iter()
                .map(|l| json!({ "at": l.at, "line": l.line }))
                .collect(),
        )
    }

    /// The `pieces` field: `[{ title }]`.
    pub fn pieces(&self) -> Value {
        Value::Array(self.pieces.iter().map(|t| json!({ "title": t })).collect())
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

fn cut(s: &str) -> String {
    clip(s, LINE_MAX)
}

/// A file's name made plain: no folders or extension, dashes and underscores to spaces.
fn plain_name(path: &str) -> String {
    let base = path.rsplit(['/', '\\']).next().unwrap_or("");
    let stem = base.rsplit_once('.').map_or(base, |(s, _)| s);
    stem.split(['-', '_'])
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

/// The first `# ` heading in what a write or an edit puts in a file.
fn heading(input: &Value) -> Option<String> {
    ["content", "new_string"]
        .iter()
        .filter_map(|k| input.get(k).and_then(Value::as_str))
        .flat_map(str::lines)
        .find_map(|l| HEADING.captures(l).map(|c| c[1].to_string()))
}

/// The live details of a drafting session's transcript (Claude Code's stream-json lines, each with the time it was read): at most
/// the newest 12 lines and the first 12 pieces. Lines of another shape give nothing.
pub fn scan(lines: &[(i64, Value)]) -> Live {
    let mut out: Vec<Line> = Vec::new();
    let mut pieces: Vec<String> = Vec::new();
    let mut piece_at: HashMap<String, usize> = HashMap::new();
    let mut written: HashMap<String, String> = HashMap::new();
    let mut reading = false;
    for (at, line) in lines {
        let blocks = line.pointer("/message/content").and_then(Value::as_array);
        for block in blocks.into_iter().flatten() {
            if block.get("type").and_then(Value::as_str) != Some("tool_use") {
                continue;
            }
            let (name, input) = (block.get("name").and_then(Value::as_str).unwrap_or(""), &block["input"]);
            let path = input.get("file_path").and_then(Value::as_str).unwrap_or("");
            if !SPEC.is_match(path) {
                continue;
            }
            if name == "Read" {
                if !reading {
                    reading = true;
                    out.insert(
                        0,
                        Line {
                            at: *at,
                            line: "Reading the specs".into(),
                        },
                    );
                }
                continue;
            }
            if name != "Write" && name != "Edit" {
                continue;
            }
            let found = heading(input);
            match written.get(path) {
                None => {
                    let title = cut(&found.clone().unwrap_or_else(|| plain_name(path)));
                    written.insert(path.to_string(), format!("Writing {title}"));
                    out.push(Line {
                        at: *at,
                        line: cut(&format!("Writing {}", found.unwrap_or_else(|| plain_name(path)))),
                    });
                    piece_at.insert(path.to_string(), pieces.len());
                    pieces.push(title);
                }
                Some(_) => {
                    if let Some(h) = found {
                        // The same file again: its line is renamed, never added.
                        let old = written
                            .insert(path.to_string(), format!("Writing {}", cut(&h)))
                            .unwrap_or_default();
                        if let Some(l) = out.iter_mut().find(|l| l.line == old) {
                            l.line = cut(&format!("Writing {h}"));
                        }
                        if let Some(&i) = piece_at.get(path) {
                            pieces[i] = cut(&h);
                        }
                    }
                }
            }
        }
    }
    let skip = out.len().saturating_sub(MAX);
    pieces.truncate(MAX);
    Live {
        lines: out.split_off(skip),
        pieces,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(at: i64, name: &str, input: Value) -> (i64, Value) {
        (
            at,
            json!({ "type": "assistant", "message": { "content": [{ "type": "tool_use", "name": name, "input": input }] } }),
        )
    }

    fn lines(live: &Live) -> Vec<&str> {
        live.lines.iter().map(|l| l.line.as_str()).collect()
    }

    #[test]
    fn a_read_of_a_spec_then_each_spec_written_make_the_lines_and_pieces() {
        let live = scan(&[
            tool(1, "Read", json!({ "file_path": "src/x.rs" })),
            tool(2, "Read", json!({ "file_path": "docs/specs/agent.md" })),
            tool(
                3,
                "Write",
                json!({ "file_path": "/w/docs/specs/two-parts.md", "content": "intro" }),
            ),
            tool(
                4,
                "Write",
                json!({ "file_path": "src/y.rs", "content": "# Not a spec" }),
            ),
            tool(
                5,
                "Edit",
                json!({ "file_path": "/w/docs/specs/two-parts.md", "new_string": "# Two parts\nbody" }),
            ),
        ]);
        assert_eq!(lines(&live), ["Reading the specs", "Writing Two parts"]);
        assert_eq!(live.pieces, ["Two parts"]);
        assert_eq!(live.lines[0].at, 2);
        assert_eq!(live.progress()[1]["line"], "Writing Two parts");
        assert_eq!(live.pieces(), json!([{ "title": "Two parts" }]));
    }

    #[test]
    fn a_file_without_a_heading_is_named_plainly_and_a_read_after_writing_still_goes_first() {
        let live = scan(&[
            tool(
                1,
                "Write",
                json!({ "file_path": "docs/specs/dark_mode-ui.md", "content": "x" }),
            ),
            tool(2, "Read", json!({ "file_path": "docs/specs/a.md" })),
        ]);
        assert_eq!(lines(&live), ["Reading the specs", "Writing dark mode ui"]);
    }

    #[test]
    fn at_most_twelve_and_a_hundred_characters_each() {
        let many: Vec<_> = (0..15)
            .map(|i| {
                tool(
                    i,
                    "Write",
                    json!({ "file_path": format!("docs/specs/s{i}.md"), "content": "x" }),
                )
            })
            .collect();
        let live = scan(&many);
        assert_eq!((live.lines.len(), live.pieces.len()), (12, 12));
        assert_eq!(live.lines[11].line, "Writing s14");
        let long = scan(&[tool(
            1,
            "Write",
            json!({ "file_path": "docs/specs/x.md", "content": format!("# {}", "y".repeat(200)) }),
        )]);
        assert!(long.lines[0].line.chars().count() <= LINE_MAX && long.pieces[0].chars().count() <= LINE_MAX);
    }

    #[test]
    fn lines_of_another_shape_give_nothing() {
        assert!(scan(&[(1, json!({ "type": "result" })), (2, json!("text"))]).is_empty());
    }
}
