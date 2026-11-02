//! The item, diff range and stage 1 evidence every review-stage session's prompt needs
//! (docs/specs/agent/06.3-review.md): a scout (stage 2, `crate::scouts`) and the final reviewer
//! (stage 3, `crate::reviewer`) both read the same item id/title/slice, spec refs, slice
//! description, diff range, re-review note and checks text. Factored out so each one's own
//! context struct holds or embeds one [`ReviewContext`] instead of repeating these fields and the
//! `render_prompt` vars built from them.
pub struct ReviewContext<'a> {
    pub item_id: &'a str,
    pub title: &'a str,
    pub slice: u32,
    /// Joined with ", ", or "(none)" when the item links no specs.
    pub spec_refs: &'a str,
    pub slice_description: &'a str,
    /// The range a scout or the reviewer reads with `git diff <range> -- <files>`.
    pub range: &'a str,
    /// The re-review note (`scope` in `review.ts`), or "" for a first review.
    pub scope: &'a str,
    pub checks: &'a str,
}

impl ReviewContext<'_> {
    /// This context's own `render_prompt` vars; the caller appends whatever else its own template
    /// needs.
    pub fn vars(&self) -> Vec<(&'static str, String)> {
        vec![
            ("itemId", self.item_id.to_string()),
            ("title", self.title.to_string()),
            ("slice", self.slice.to_string()),
            ("specRefs", self.spec_refs.to_string()),
            ("sliceDescription", self.slice_description.to_string()),
            ("range", self.range.to_string()),
            ("scope", self.scope.to_string()),
            ("checks", self.checks.to_string()),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx<'a>() -> ReviewContext<'a> {
        ReviewContext {
            item_id: "my-item",
            title: "My item",
            slice: 2,
            spec_refs: "(none)",
            slice_description: "desc",
            range: "origin/main...HEAD",
            scope: "",
            checks: "✓ test",
        }
    }

    #[test]
    fn vars_includes_every_common_field() {
        assert_eq!(
            ctx().vars(),
            vec![
                ("itemId", "my-item".to_string()),
                ("title", "My item".to_string()),
                ("slice", "2".to_string()),
                ("specRefs", "(none)".to_string()),
                ("sliceDescription", "desc".to_string()),
                ("range", "origin/main...HEAD".to_string()),
                ("scope", "".to_string()),
                ("checks", "✓ test".to_string()),
            ]
        );
    }
}
