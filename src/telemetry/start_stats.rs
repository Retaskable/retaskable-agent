//! How fresh implementer sessions get going (agent.md section 6.2, agent-telemetry.md section 5):
//! turns and calls before the first edit and the share of the session's cost spent there. Reads
//! the session transcripts the agent tees to `.retaskable/projects/<id>/runs/<item>/<session>.jsonl`,
//! the only place a turn's tool calls and usage are kept.
use std::collections::HashMap;
use std::path::Path;

use serde_json::Value;

const EDITS: [&str; 4] = ["Edit", "Write", "MultiEdit", "NotebookEdit"];

#[derive(Debug, PartialEq)]
pub struct StartStats {
    pub project: String,
    pub item: String,
    pub turns: usize,
    pub pre_turns: usize,
    pub pre_calls: usize,
    pub single_call_pre: usize,
    pub pre_cost_share: f64,
    pub usd: f64,
}

struct Turn {
    calls: Vec<String>,
    cost: f64,
}

fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn has_parent(v: &Value) -> bool {
    matches!(v.get("parent_tool_use_id"), Some(Value::String(s)) if !s.is_empty())
}

/// One transcript's numbers, or `None` when it isn't a finished Claude implementer session that
/// made an edit. Each turn's cost weighs its usage by price (cache writes 1.25x, reads 0.1x,
/// output 5x), counted once per message.
pub fn start_stats(project: &str, item: &str, lines: &[Value]) -> Option<StartStats> {
    let meta = lines.iter().find(|l| str_field(l, "type") == Some("retaskable_meta"))?;
    let result = lines.iter().rev().find(|l| str_field(l, "type") == Some("result"))?;
    let role = str_field(meta, "role");
    let engine = str_field(meta, "engine").unwrap_or("claude");
    if role != Some("implementer") || engine != "claude" {
        return None;
    }
    let mut order: Vec<String> = Vec::new();
    let mut turns: HashMap<String, Turn> = HashMap::new();
    for l in lines {
        if str_field(l, "type") != Some("assistant") || has_parent(l) {
            continue;
        }
        let Some(message) = l.get("message") else { continue };
        let Some(id) = message.get("id").and_then(Value::as_str) else {
            continue;
        };
        let entry = turns.entry(id.to_string()).or_insert_with(|| {
            order.push(id.to_string());
            Turn {
                calls: Vec::new(),
                cost: 0.0,
            }
        });
        if let Some(content) = message.get("content").and_then(Value::as_array) {
            for c in content {
                if str_field(c, "type") == Some("tool_use")
                    && let Some(name) = str_field(c, "name")
                {
                    entry.calls.push(name.to_string());
                }
            }
        }
        let usage = message.get("usage");
        let field = |k: &str| usage.and_then(|u| u.get(k)).and_then(Value::as_f64).unwrap_or(0.0);
        entry.cost = field("input_tokens")
            + field("cache_creation_input_tokens") * 1.25
            + field("cache_read_input_tokens") * 0.1
            + field("output_tokens") * 5.0;
    }
    let all: Vec<&Turn> = order
        .iter()
        .filter_map(|id| turns.get(id))
        .filter(|t| !t.calls.is_empty())
        .collect();
    let first = all
        .iter()
        .position(|t| t.calls.iter().any(|c| EDITS.contains(&c.as_str())))?;
    let pre = &all[..first];
    let cost = |ts: &[&Turn]| ts.iter().map(|t| t.cost).sum::<f64>();
    let cost_all = cost(&all);
    Some(StartStats {
        project: project.to_string(),
        item: item.to_string(),
        turns: all.len(),
        pre_turns: pre.len(),
        pre_calls: pre.iter().map(|t| t.calls.len()).sum(),
        single_call_pre: pre.iter().filter(|t| t.calls.len() == 1).count(),
        pre_cost_share: if cost_all != 0.0 { cost(pre) / cost_all } else { 0.0 },
        usd: result.get("total_cost_usd").and_then(Value::as_f64).unwrap_or(0.0),
    })
}

/// The newest `count` sessions' numbers under `data_dir`, newest first; chores and the agent's
/// own runs (`_planner`, `_playtests`…) left out.
pub fn read_start_stats(data_dir: &Path, count: usize, project: Option<&str>) -> Vec<StartStats> {
    let mut files: Vec<(String, String, std::path::PathBuf, std::time::SystemTime)> = Vec::new();
    let projects_dir = data_dir.join("projects");
    let Ok(projects) = std::fs::read_dir(&projects_dir) else {
        return Vec::new();
    };
    for p in projects.flatten() {
        let p_name = p.file_name().to_string_lossy().into_owned();
        if let Some(want) = project
            && p_name != want
        {
            continue;
        }
        let runs_dir = projects_dir.join(&p_name).join("runs");
        let Ok(items) = std::fs::read_dir(&runs_dir) else {
            continue;
        };
        for item in items.flatten() {
            let item_name = item.file_name().to_string_lossy().into_owned();
            if item_name.starts_with('_') || item_name.starts_with("chore-") {
                continue;
            }
            let dir = runs_dir.join(&item_name);
            if !dir.is_dir() {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for f in entries.flatten() {
                let name = f.file_name().to_string_lossy().into_owned();
                if !name.ends_with(".jsonl") {
                    continue;
                }
                let path = dir.join(&name);
                let mtime = std::fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::UNIX_EPOCH);
                files.push((p_name.clone(), item_name.clone(), path, mtime));
            }
        }
    }
    files.sort_by_key(|f| std::cmp::Reverse(f.3));
    let mut out = Vec::new();
    for (project, item, file, _) in files {
        if out.len() >= count {
            break;
        }
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let lines: Vec<Value> = text
            .split(['\n', '\r'])
            .filter(|l| !l.is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        if let Some(s) = start_stats(&project, &item, &lines) {
            out.push(s);
        }
    }
    out
}

fn median(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    let mut s = xs.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = s.len();
    if n % 2 == 1 {
        s[(n - 1) / 2]
    } else {
        (s[n / 2 - 1] + s[n / 2]) / 2.0
    }
}

fn pct(x: f64) -> String {
    format!("{}%", (x * 100.0).round() as i64)
}

/// Matches JS's default `Number` → `String` for the plain values this report prints: no `.0` on a
/// whole number, otherwise its ordinary decimal form.
fn js_num(x: f64) -> String {
    if x == x.trunc() {
        format!("{}", x as i64)
    } else {
        format!("{x}")
    }
}

/// The report: one line per session, then the medians.
pub fn start_stats_text(stats: &[StartStats]) -> String {
    if stats.is_empty() {
        return "No finished implementer sessions with an edit.".to_string();
    }
    let rows: Vec<String> = stats
        .iter()
        .map(|s| {
            let label: String = format!("{}/{}", s.project, s.item).chars().take(44).collect();
            format!(
                "{label:<44} {:>3} of {:<4} turns before the first edit, {:>3} calls, {:>4} of ${:.2}",
                s.pre_turns,
                s.turns,
                s.pre_calls,
                pct(s.pre_cost_share),
                s.usd
            )
        })
        .collect();
    let m = |f: &dyn Fn(&StartStats) -> f64| median(&stats.iter().map(f).collect::<Vec<_>>());
    let summary = format!(
        "{} sessions. Medians before the first edit: {} turns of {}, {} calls, {} of turns making one call, {} of the cost; ${:.2} a session.",
        stats.len(),
        js_num(m(&|s| s.pre_turns as f64)),
        js_num(m(&|s| s.turns as f64)),
        js_num(m(&|s| s.pre_calls as f64)),
        pct(m(&|s| if s.pre_turns > 0 {
            s.single_call_pre as f64 / s.pre_turns as f64
        } else {
            0.0
        })),
        pct(m(&|s| s.pre_cost_share)),
        m(&|s| s.usd),
    );
    let mut lines = rows;
    lines.push(String::new());
    lines.push(summary);
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn turn(id: &str, tools: &[&str], cache_read: f64, parent: Option<&str>) -> Value {
        json!({
            "type": "assistant",
            "parent_tool_use_id": parent,
            "message": {
                "id": id,
                "content": tools.iter().map(|name| json!({ "type": "tool_use", "name": name })).collect::<Vec<_>>(),
                "usage": { "cache_read_input_tokens": cache_read },
            },
        })
    }

    fn meta() -> Value {
        json!({ "type": "retaskable_meta", "role": "implementer", "engine": "claude" })
    }

    fn result() -> Value {
        json!({ "type": "result", "total_cost_usd": 1.5 })
    }

    fn with(mut base: Value, k: &str, v: Value) -> Value {
        base[k] = v;
        base
    }

    #[test]
    fn turns_calls_and_cost_before_the_first_edit_a_subagents_turns_and_repeated_lines_counted_once() {
        let lines = vec![
            meta(),
            turn("a", &["Read"], 100.0, None),
            turn("b", &["Grep", "Read"], 100.0, None),
            turn("b", &["Read"], 100.0, None),
            turn("x", &["Read"], 999.0, Some("toolu_1")),
            turn("c", &["Edit"], 200.0, None),
            turn("d", &[], 300.0, None),
            result(),
        ];
        let s = start_stats("p", "item", &lines).unwrap();
        assert_eq!(s.project, "p");
        assert_eq!(s.item, "item");
        assert_eq!(s.turns, 3);
        assert_eq!(s.pre_turns, 2);
        assert_eq!(s.pre_calls, 4);
        assert_eq!(s.single_call_pre, 1);
        assert_eq!(s.pre_cost_share, 0.5);
        assert_eq!(s.usd, 1.5);
    }

    #[test]
    fn only_a_finished_claude_implementer_session_that_made_an_edit() {
        assert_eq!(
            start_stats(
                "p",
                "i",
                &[
                    with(meta(), "role", json!("reviewer")),
                    turn("a", &["Edit"], 1.0, None),
                    result()
                ]
            ),
            None
        );
        assert_eq!(
            start_stats(
                "p",
                "i",
                &[
                    with(meta(), "engine", json!("cursor")),
                    turn("a", &["Edit"], 1.0, None),
                    result()
                ]
            ),
            None
        );
        assert_eq!(
            start_stats("p", "i", &[meta(), turn("a", &["Edit"], 1.0, None)]),
            None,
            "no result yet"
        );
        assert_eq!(
            start_stats("p", "i", &[meta(), turn("a", &["Read"], 1.0, None), result()]),
            None,
            "no edit"
        );
    }

    #[test]
    fn the_reports_medians() {
        let s = |pre_turns: usize, turns: usize| StartStats {
            project: "p".to_string(),
            item: "i".to_string(),
            turns,
            pre_turns,
            pre_calls: pre_turns,
            single_call_pre: pre_turns,
            pre_cost_share: 0.4,
            usd: 2.0,
        };
        let text = start_stats_text(&[s(10, 30), s(20, 40)]);
        assert!(
            text.contains("2 sessions. Medians before the first edit: 15 turns of 35, 15 calls, 100% of turns making one call, 40% of the cost; $2.00 a session."),
            "{text}"
        );
        assert!(start_stats_text(&[]).contains("No finished implementer sessions"));
    }
}
