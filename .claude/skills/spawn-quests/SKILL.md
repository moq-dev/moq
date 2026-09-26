---
name: spawn-quests
description: Spawn background agents work on quests in parallel.
---

Before you begin, read `quest/AGENTS.md` completely.

Your goal is to execute, plan, and/or merge quests in parallel.
If you are unsure of the best course of action, ask the user for clarification before proceeding.

The scope consists of all ready quests that are not claimed.
Use the argument (if provided) to filter to specific quests/questlines.
Main's tree lags its lines: a child finished on its line branch, or on `dev`, still looks ready from `main`.
Judge readiness from each line branch's own quest directory, and treat a quest deleted on `dev` as done.
Inspect any blocked quests, and determine if they can be unblocked.
A line whose `Quests` list has emptied is a ready quest too: finishing it marks the line's PR ready.

Recommend an action for each quest: /start-quest, /plan-quests, skip, or delete.
Start the quests you'd start with no open question right away.
Interactively prompt the user about the rest, a few per prompt, each with a short summary and your recommendation.

Spawn a background sub-agent for each /start-quest.
Create a fresh worktree on the base `quest branch` prints, creating that line branch first if it is missing.
Agents share no writable files: each keeps its scratch files in its own worktree's `.scratch/`, and anything you hand every agent goes in its prompt, not a shared file.
Each agent blocks on its own checks and reports back only when done or blocked.
Limit the concurrency to at most N agents in parallel, where N is half the number of physical CPU cores.
Other sessions share this machine: hold new agents while the load average exceeds the core count.

Report each sub-agent's final status, staying silent on interim notifications, but do not monitor their PRs.
Prompt the user if they want to /plan-quests for any suggested follow-ups.

Run /plan-quests for any selected quests in the foreground.
Perform any research and monitoring in the background.

Finally, create a PR for any created/updated quests.
