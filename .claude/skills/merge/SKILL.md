---
name: merge
description: Merge a GitHub PR once reviews and CI pass.
---

Land a pull request.
If unsure about any course of action, pause and interactively prompt the user for guidance.

- Parse the arguments to determine the PR number, otherwise resolve it from the current context/branch.
- Fix any issues with the PR, such as merge conflicts and failing CI checks.
- If a draft, flip it to "Ready for Review" then wait for at least one automated review to complete.
- Fix any review feedback you agree with, and leave a comment if you disagree.

If everything looks good, enable auto-merge and leave a summary of the changes made.

After GitHub confirms the PR is merged or closed, run `just clean` in its local
checkout if that recipe exists, once this task's checks and builds have stopped.
Enabling auto-merge is not completion. Preserve another task's active processes;
report a refused or failed cleanup without changing the PR outcome. Leave the
checkout itself for the host's worktree cleanup timer to retire after you exit.
