---
name: close
description: Close a GitHub PR.
---

Abandon the specified PR.

If this PR was completing a quest, takeover the PR to delete the quest instead (without the other changes).
If this PR was not completing a quest, close the PR as-is.

After GitHub confirms the PR is merged or closed, run `just clean` in its local
checkout if that recipe exists, once this task's checks and builds have stopped.
Enabling auto-merge is not completion. Preserve another task's active processes;
report a refused or failed cleanup without changing the PR outcome. Leave the
checkout itself for the host's worktree cleanup timer to retire after you exit.
