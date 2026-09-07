---
name: spawn-quests
description: Spawn background agents work on quests in parallel. 
---

Before you begin, read `quest/CLAUDE.md` completely.

The scope consists of all unblocked quests that are not claimed and have no blockers.
Use the argument (if provided) to filter to specific quests/questlines.
Report which quests are not ready to be worked on and why.

For each quest, spawn a sub-agent to /start-quest.
Limit the concurrency to at most N agents in parallel, where N is half the number of CPU cores.
Monitor the sub-agents and report the status.
Don't monitor or merge any PRs; that's up to the agent.
