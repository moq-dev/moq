---
name: start-quest
description: Start work on a quest.
---

Before you begin, read `quest/CLAUDE.md` completely.

You goal is to implement the quest, or as much of it as possible, and create a PR.
The argument is the quest to work on.

If you run into issues, or doubt the direction, you may create new quests or update/delete the selected one.
Scheduling a /plan-quest session is a good idea if the plan/goal is unclear.

Start by claiming the quest via creating a placeholder commit, pushing it to the origin with the correct branch.
Implement the quest until it is complete, or some blocker is hit, then create a PR.

After submitting the PR:
- Monitor it for CI failures and reviews.
- Address any automated review findings (Codex/CodeRabbit) you agree with. Turn down any you disagree with with a comment.
- Push any changes you made to the PR, updating the summary if needed.

Wait for at least one automated reviewer and if you're confident, enable auto-merge.
Otherwise, leave the PR open and the user will decide if it should be merged.
