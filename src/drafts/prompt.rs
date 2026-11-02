//! Fills `prompts/drafter.md` (spec 06.5 step 2; `prompt("drafter", …)` and `titlesText` in
//! Retaskable.com's `agent/src/drafts.ts`).
use anyhow::Result;

use crate::prompt::render_prompt;

/// `titlesText` (drafts.ts): one parsed title per line with its theme beside it, or, with none
/// (**Add spec**, gallery feedback, a specs chore), a line telling the drafter to make the cut and
/// the themes itself. `themes` is used only when there's one per title.
pub fn titles_text(titles: &[String], themes: &[String]) -> String {
    if titles.is_empty() {
        return "None were given: split the request yourself (step 2), classify each piece (step 3), title each entry from its piece, and give each piece its theme (step 4).".to_string();
    }
    let n = if titles.len() == 1 {
        "one piece".to_string()
    } else {
        format!("{} pieces", titles.len())
    };
    let themed = themes.len() == titles.len();
    let lines: Vec<String> = titles
        .iter()
        .enumerate()
        .map(|(i, t)| {
            if themed {
                format!("- {t} (theme: {})", themes[i])
            } else {
                format!("- {t}")
            }
        })
        .collect();
    format!(
        "The chat parsed this request into {n}{}. Write one backlog entry per title, in this order, with that title, and {}:\n{}",
        if themed { ", each with its theme" } else { "" },
        if themed {
            "use the theme beside it"
        } else {
            "give each piece its theme (step 4)"
        },
        lines.join("\n")
    )
}

/// Fills the drafter's template for a fresh session. `titles` is [`titles_text`]'s output.
pub fn build_prompt(template: &str, request: &str, titles: &str, branch: &str, base_branch: &str) -> Result<String> {
    render_prompt(
        template,
        &[
            ("request", request.to_string()),
            ("titles", titles.to_string()),
            ("branch", branch.to_string()),
            ("baseBranch", base_branch.to_string()),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn titles_text_lists_each_title_with_its_theme() {
        let t = titles_text(&s(&["Dark mode", "Export"]), &s(&["Look", "Data"]));
        assert!(t.contains("2 pieces, each with its theme"));
        assert!(t.contains("- Dark mode (theme: Look)\n- Export (theme: Data)"));
        let plain = titles_text(&s(&["Dark mode"]), &[]);
        assert!(plain.contains("one piece") && plain.contains("- Dark mode") && !plain.contains("theme:"));
    }

    #[test]
    fn titles_text_with_none_tells_the_drafter_to_make_the_cut() {
        let t = titles_text(&[], &[]);
        assert!(t.starts_with("None were given: split the request yourself"));
    }

    #[test]
    fn the_filled_prompt_has_the_request_titles_and_base_branch_and_no_placeholders() {
        let template = include_str!("../../prompts/drafter.md");
        let titles = titles_text(&s(&["Dark mode"]), &s(&["Look"]));
        let out = build_prompt(template, "Add dark mode please", &titles, "retaskable/spec-1-x", "main").unwrap();
        assert!(out.contains("Add dark mode please"));
        assert!(out.contains("- Dark mode (theme: Look)"));
        assert!(out.contains("`origin/main`"));
        assert!(out.contains("retaskable/spec-1-x"));
        assert!(!out.contains("{{"));
    }
}
