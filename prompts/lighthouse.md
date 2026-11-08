{{tag}} A Lighthouse run on {{url}} found new problems. Turn them into a spec.

## Pages and scores

{{pages}}

## New findings

Each finding is `<page> <preset> <audit id>` (or `score <category>`), with Lighthouse's own words and up to 3 items it names.

{{findings}}

## The repo's specs

{{specsIndex}}

## What to write

Write **one** spec that covers these findings, or extend an existing spec above that already owns the area. It must say:

1. **What's wrong**, with the numbers above (scores, display values, items).
2. **What to change**, concretely enough to build.
3. **How to check it's fixed**: run Lighthouse on the same page and preset and look for the audit or score named in the finding.

Staging is blocked from indexing on purpose, so ignore anything about crawling or SEO.
