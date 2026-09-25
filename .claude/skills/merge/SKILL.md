---
name: merge
description: Merge a GitHub PR once reviews and CI pass.
---

Land a pull request.
If unsure about any course of action, pause and interactively prompt the user for guidance.

- Parse the arguments to determine the PR number, otherwise resolve it from the current context/branch.
- Fix any minor issues with the PR, such as merge conflicts and failing CI checks.
- If a draft, flip it to "Ready for Review" then wait for at least one automated review to complete.
- Fix any review feedback you agree with, and leave a comment if you disagree.

If everything looks good, enable auto-merge and post a summary of the changes made.
You don't need to address everything.

You may repeat any of the above steps on new CI results or review feedback.
Abort on any major issues or blockers, or if little progress is being made.
