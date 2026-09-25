---
name: start-quest
description: Start work on a quest.
---

Before you begin, read `quest/CLAUDE.md` completely.

Your goal is to implement the quest, or as much of it as possible, and create a PR.
The argument is the quest to work on.

If you are unsure on the best course of action, ask the user for direction.

Confirm the quest is ready and unclaimed.
Claim it as `quest/CLAUDE.md` describes: `quest branch` names the branch and its bases, missing line branches get a draft PR, and the quest branch gets an empty commit.
Implement the quest until it is complete, or some blocker is hit, then create a PR against the base.
Keep scratch files (PR body, logs, notes) in the worktree's gitignored `.scratch/`.
Never write to or clean up a directory other agents share, such as a session scratchpad.
Summarize the notable changes for the user.
