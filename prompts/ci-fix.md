Pull request #{{pr}} "{{title}}" (branch {{branch}}) is failing CI because of its own change. Fix it in this worktree, which is on the branch.

The last CI run failed because:

{{what}}
{{earlier}}
Commit the fix. Do not push, do not open or change a pull request, do not force anything. Run the checks that failed before you finish. Return status "pr_opened" with a summary once it is committed: say briefly what you changed and whether the checks passed.
