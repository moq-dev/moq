---
name: spawn-quests
description: Spawn background agents work on quests in parallel. 
---

Before you begin, read `quest/CLAUDE.md` completely.

The scope consists of all unblocked quests that are not claimed and have no blockers.
Use the argument (if provided) to filter to specific quests/questlines.
Report which quests are not ready to be worked on and why.

For each quest, interactively prompt the user if we should work on the quest, scope it further with /plan-quest, lower the priority, or skip it.

For each quest to work on, spawn a background sub-agent to /start-quest.
Determine the base branch for the quest and create a fresh worktree.
Limit the concurrency to at most N agents in parallel, where N is half the number of CPU cores.
Keep prompting the user if needed while these agents work in the background.

Monitor the sub-agents and report their final status, but do not monitor their PRs.
Prompt the user if they want to /plan-quest for any suggested follow-ups.

Finally, create a PR if there are any created/updated quests.
