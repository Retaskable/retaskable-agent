Pull request #{{pr}} "{{title}}" (branch {{branch}}) conflicts with its base branch, so it cannot be merged. Fix it in this worktree, which is on the branch. `origin` is already fetched.

{{what}}
{{earlier}}
Resolve each conflict keeping the intent of both sides: what this pull request changes and what the base changed. Never rebase and never force anything. Run the project's checks, then commit the merge. Do not push, do not open or change a pull request. Return status "pr_opened" with a summary once it is committed: say briefly what you resolved and whether the checks passed.
