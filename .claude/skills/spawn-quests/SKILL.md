---
name: spawn-quests
description: Spawn background agents work on quests in parallel. 
---

Before you begin, read `quest/CLAUDE.md` completely.

Your goal is to execute, plan, and/or merge quests in parallel.
If you are unsure of the best course of action, ask the user for clarification before proceeding.

The scope consists of all unblocked quests that are not claimed and have no blockers.
Use the argument (if provided) to filter to specific quests/questlines.
Inspect any blocked quests, and determine if they can be unblocked.

For each quest, interactively prompt the user if:

1. /start-quest
2. /plan-quests
3. skip it
4. delete it

Include a recommended option.

For each quest to work on, spawn a background sub-agent to /start-quest.
Determine the base branch for the quest and create a fresh worktree.
Limit the concurrency to at most N agents in parallel, where N is half the number of physical CPU cores.

Monitor the sub-agents and report their final status, but do not monitor their PRs.
Prompt the user if they want to /plan-quests for any suggested follow-ups.

Run /plan-quests for any selected quests in the foreground.
Perform any research and monitoring in the background.

Finally, create a PR if there are any created/updated quests.
