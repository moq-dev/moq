---
name: start-quest
description: Start work on a quest.
---

Before you begin, read `quest/AGENTS.md` completely.

Your goal is to implement the quest, or as much of it as possible, and create a draft PR.
The argument is the quest to work on.

If you are unsure on the best course of action, ask the user for direction.

Confirm the quest is ready and unclaimed.
Claim it as `quest/AGENTS.md` describes: `quest branch` names the branch and its bases, missing line branches get a draft PR, and the quest branch gets an empty commit.

Implement the quest until it is complete, or some blocker is hit, then create a PR against the base.
Keep scratch files (PR body, logs, notes) in the worktree's gitignored `.scratch/`.
Never write to or clean up a directory other agents share, such as a session scratchpad.

When done, summarize any issues encounted, and suggest potential follow-up.
List every open decision (naming, API shape, branch, blockers, manual steps) with a recommendation, and prompt the user interactively when you can.
Keep the PR a draft until the user has confirmed every decision, then mark it ready for review.
